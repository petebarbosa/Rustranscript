#!/usr/bin/env python3
"""Worker de transcrição (protocolo JSON-lines v1; tipos em crates/core/src/transcription/protocol.rs).

- stdout é SÓ protocolo (uma mensagem JSON por linha). O fd 1 original é duplicado para o protocolo e o fd 1
  passa a apontar para o stderr, então `print` e bibliotecas nativas nunca corrompem o canal. Logs: stderr.
- Um pedido por vez. Uma thread lê o stdin (para o `cancel` chegar durante um pedido); EOF no stdin = o pai
  morreu ou fechou o pipe -> `os._exit(0)` na hora (o ctranslate2 não é interrompível no meio de uma janela).
- Carga preguiçosa: o `hello` sai antes de importar qualquer biblioteca pesada; o modelo carrega no 1º pedido.
- `--fake` (ou TARY_FAKE_WORKER): modo determinístico, só biblioteca padrão (regra em
  TARY_FAKE_DELAY_MS: pausa em ms entre segmentos; `slow` em TARY_FAKE_WORKER = 300).
"""
import json
import math
import os
import queue
import sys
import threading
import time
import traceback
from importlib import metadata

PROTOCOL = 1
WORKER_VERSION = "0.3.1"
REQUESTS = ("transcribe", "diarize", "energy")
SAMPLE_RATE = 16000
REQUIRED = object()
# junção de falantes pela voz (#25)
MERGE_SIMILARITY = 0.75  # cosseno mínimo entre centroides para ser a mesma pessoa
MIN_SPEAKER_S = 15.0     # abaixo disso de fala o grupo não vira pessoa: junta ao mais parecido
EMB_TURNS = 20           # turnos por grupo na amostra do centroide
EMB_MIN_TURN_S = 1.0
EMB_MAX_TURN_S = 10.0

_proto = None  # stream do protocolo (fd 1 original, duplicado)
_proto_lock = threading.Lock()


# ---------------------------------------------------------------- saída / log

def attach_protocol_stream():
    """Reserva o fd 1 original para o protocolo e redireciona o fd 1 para o stderr."""
    global _proto
    sys.stdout.flush()
    _proto = os.fdopen(os.dup(1), "w", buffering=1, encoding="utf-8", newline="\n")
    os.dup2(2, 1)


def send(msg):
    line = json.dumps(msg, ensure_ascii=False, separators=(",", ":"), allow_nan=False) + "\n"
    try:
        with _proto_lock:
            (_proto or sys.stdout).write(line)
            (_proto or sys.stdout).flush()
    except (OSError, ValueError):
        os._exit(0)  # pipe fechado: o pai já não existe


def log(*args):
    print(*args, file=sys.stderr, flush=True)


def send_error(rid, code, detail, fatal=False):
    send({"type": "error", "id": rid, "code": code, "detail": str(detail)[:500], "fatal": fatal})


def version(package):
    try:
        return metadata.version(package)
    except metadata.PackageNotFoundError:
        return None


def hello(fake):
    return {
        "type": "hello",
        "protocol": PROTOCOL,
        "worker": WORKER_VERSION,
        "pid": os.getpid(),
        "python": "%d.%d.%d" % sys.version_info[:3],
        "faster_whisper": None if fake else version("faster-whisper"),
        "sherpa_onnx": None if fake else version("sherpa-onnx"),
        "fake": fake,
    }


# ---------------------------------------------------------------- estado / cancelamento

class State:
    def __init__(self):
        self.lock = threading.Lock()
        self.live = set()        # ids de pedidos na fila ou em execução
        self.cancelled = set()   # ids com `cancel` recebido enquanto vivos
        self.stopping = False    # `shutdown` recebido: o pedido corrente também termina
        self.wake = threading.Event()


STATE = State()


class Ctx:
    """Contexto de um pedido: checagem de cancelamento e espera interrompível."""

    def __init__(self, rid):
        self.id = rid

    def cancelled(self):
        with STATE.lock:
            return STATE.stopping or self.id in STATE.cancelled

    def pause(self, seconds):
        if seconds > 0:
            STATE.wake.wait(seconds)
            if not self.cancelled():
                STATE.wake.clear()


class BadRequest(Exception):
    pass


class WorkerError(Exception):
    def __init__(self, code, detail, fatal=False):
        super().__init__(detail)
        self.code, self.detail, self.fatal = code, detail, fatal


def field(msg, key, kind, default=REQUIRED):
    """Lê um campo tipado do pedido; ausente/null -> `default` (ou BadRequest se obrigatório)."""
    v = msg.get(key)
    if v is None:
        if default is REQUIRED:
            raise BadRequest("missing field: %s" % key)
        return default
    if kind is float:
        ok = isinstance(v, (int, float)) and not isinstance(v, bool)
    elif kind is int:
        ok = isinstance(v, int) and not isinstance(v, bool)
    else:
        ok = isinstance(v, kind)
    if not ok:
        raise BadRequest("invalid field: %s" % key)
    return float(v) if kind is float else v


def parse_mute(v):
    """`mute`: lista de [início, fim] em segundos do ARQUIVO da trilha (cortes de áudio, #23); ausente = nada."""
    if v is None:
        return []
    if not isinstance(v, list):
        raise BadRequest("invalid field: mute")
    out = []
    for r in v:
        ok = isinstance(r, (list, tuple)) and len(r) == 2 and all(
            isinstance(x, (int, float)) and not isinstance(x, bool) and math.isfinite(x) for x in r)
        if not ok:
            raise BadRequest("invalid field: mute")
        if r[1] > r[0]:
            out.append((float(r[0]), float(r[1])))
    return out


def parse_request(msg):
    kind = msg["type"]
    p = {"audio": field(msg, "audio", str), "mute": parse_mute(msg.get("mute"))}
    if kind == "transcribe":
        p.update(
            track=field(msg, "track", str), model_dir=field(msg, "model_dir", str),
            language=field(msg, "language", str, None), hotwords=field(msg, "hotwords", str, None),
            beam_size=field(msg, "beam_size", int, 5), threads=field(msg, "threads", int, 0),
            word_timestamps=field(msg, "word_timestamps", bool, False),
            vad_min_silence_ms=field(msg, "vad_min_silence_ms", int, 500),
            start_s=field(msg, "start_s", float, 0.0),
        )
        if p["beam_size"] < 1 or p["threads"] < 0 or p["start_s"] < 0:
            raise BadRequest("out of range: beam_size/threads/start_s")
    elif kind == "diarize":
        p.update(
            seg_model=field(msg, "seg_model", str), emb_model=field(msg, "emb_model", str),
            threshold=field(msg, "threshold", float, 0.9), threads=field(msg, "threads", int, 0),
            merge_similarity=field(msg, "merge_similarity", float, MERGE_SIMILARITY),
            min_speaker_s=field(msg, "min_speaker_s", float, MIN_SPEAKER_S),
        )
        # teto de falantes (o número que o usuário informa); `num_clusters` é o nome antigo do mesmo campo
        mx = field(msg, "max_speakers", int, None)
        p["max_speakers"] = (mx if mx is not None else field(msg, "num_clusters", int, None)) or None
        if p["max_speakers"] is not None and p["max_speakers"] < 1:
            p["max_speakers"] = None
        if not (math.isfinite(p["merge_similarity"]) and -1.0 <= p["merge_similarity"] <= 1.0) \
                or not (math.isfinite(p["min_speaker_s"]) and p["min_speaker_s"] >= 0):
            raise BadRequest("out of range: merge_similarity/min_speaker_s")
    else:
        p["step_ms"] = field(msg, "step_ms", int, 100)
        if p["step_ms"] < 1:
            raise BadRequest("out of range: step_ms")
    return p


# ---------------------------------------------------------------- modo falso (só stdlib)

def fake_duration(path):
    """Duração (s) pelo STREAMINFO do FLAC (20 bits de taxa, 36 de amostras totais); WAV como apoio."""
    try:
        with open(path, "rb") as f:
            head = f.read(4)
            if head == b"fLaC":
                block = f.read(4)  # 1º bloco: STREAMINFO (tipo 0), 34 bytes de dados
                if len(block) < 4 or block[0] & 0x7F != 0:
                    raise ValueError("no STREAMINFO")
                data = f.read(34)
                packed = int.from_bytes(data[10:18], "big")  # 20 taxa | 3 canais | 5 bps | 36 amostras
                return (packed & ((1 << 36) - 1)) / (packed >> 44)
        import wave
        with wave.open(path, "rb") as w:
            return w.getnframes() / w.getframerate()
    except Exception as e:  # arquivo ausente, formato inválido, taxa 0...
        raise WorkerError("audio_decode", "%s: %s" % (type(e).__name__, e))


def fake_audible(mute, start, end):
    """Parte de [start, end] que sobra fora de `mute` (do 1º ao último instante audível); None = tudo zerado.
    É o que o VAD faria com áudio zerado; mesma regra do `FakeEngine` em Rust (`audible`)."""
    pieces = [(start, end)]
    for ms, me in mute:
        nxt = []
        for a, b in pieces:
            if me <= a or ms >= b:
                nxt.append((a, b))
                continue
            nxt.extend(x for x in ((a, max(ms, a)), (min(me, b), b)) if x[1] > x[0])
        pieces = nxt
    return (pieces[0][0], pieces[-1][1]) if pieces else None


def fake_delay_s():
    raw = os.environ.get("TARY_FAKE_DELAY_MS")
    if raw is None and os.environ.get("TARY_FAKE_WORKER") == "slow":
        raw = "300"
    try:
        return max(0.0, float(raw or 0) / 1000.0)
    except ValueError:
        return 0.0


def send_cancelled(rid, n):
    send({"type": "cancelled", "id": rid, "segments": n})


def fake_transcribe(ctx, p):
    dur = fake_duration(p["audio"])
    rid, start_s = ctx.id, p["start_s"]
    delay = fake_delay_s()
    send({"type": "progress", "id": rid, "stage": "loading_model"})
    send({"type": "progress", "id": rid, "stage": "transcribe", "audio_s": min(start_s, dur), "total_s": dur})
    prefix = "eu trecho" if p["track"] == "mic" else "fala trecho"
    n, k = 0, math.ceil(start_s / 5.0)
    while k * 5 < dur:
        # progresso parcial dentro da "janela" (o worker real estima o mesmo antes de a janela terminar)
        send({"type": "progress", "id": rid, "stage": "transcribe", "audio_s": min(k * 5.0 + 2.25, dur), "total_s": dur})
        ctx.pause(delay)
        if ctx.cancelled():
            return send_cancelled(rid, n)
        start, end = k * 5.0, min(k * 5.0 + 4.5, dur)
        heard = fake_audible(p["mute"], start, end)
        if heard is None:  # áudio zerado pelos cortes: nada a transcrever (o VAD real pula)
            send({"type": "progress", "id": rid, "stage": "transcribe", "audio_s": end, "total_s": dur})
            k += 1
            continue
        start, end = heard
        text = "%s %d" % (prefix, k)
        seg = {"type": "segment", "id": rid, "start": start, "end": end, "text": text}
        if p["word_timestamps"]:
            ws = text.split(" ")
            step = (end - start) / len(ws)
            seg["words"] = [[start + i * step, start + (i + 1) * step, w] for i, w in enumerate(ws)]
        send(seg)
        send({"type": "progress", "id": rid, "stage": "transcribe", "audio_s": end, "total_s": dur})
        n, k = n + 1, k + 1
    send({"type": "progress", "id": rid, "stage": "transcribe", "audio_s": dur, "total_s": dur})  # fecha em 100 %
    send({"type": "result", "id": rid, "segments": n, "seconds": 0.0, "language": p["language"] or "pt"})


def fake_diarize(ctx, p):
    dur = fake_duration(p["audio"])
    rid, delay = ctx.id, fake_delay_s()
    n_turns = math.ceil(dur / 15.0)
    nspk = 3 if (p["max_speakers"] or 0) >= 3 else 2
    send({"type": "progress", "id": rid, "stage": "diarize_segmentation"})
    turns = []
    for i in range(n_turns):
        ctx.pause(delay)
        if ctx.cancelled():
            return send_cancelled(rid, 0)
        start = i * 15.0
        heard = fake_audible(p["mute"], start, min(start + 15.0, dur))
        if heard is not None:
            turns.append({"start": heard[0], "end": heard[1], "speaker": i % nspk})
        send({"type": "progress", "id": rid, "stage": "diarize_embedding", "done": i + 1, "total": n_turns})
    send({"type": "result", "id": rid, "turns": turns, "speakers": len({t["speaker"] for t in turns})})


def fake_energy(ctx, p):
    dur = fake_duration(p["audio"])
    if ctx.cancelled():
        return send_cancelled(ctx.id, 0)
    # sem o tipo da trilha no pedido: o nome do arquivo decide (mic.flac = -25 dB, abaixo do sys em -20 dB)
    level = -25.0 if "mic" in os.path.basename(p["audio"]).lower() else -20.0
    n = math.ceil(dur * 1000 / p["step_ms"])
    step = p["step_ms"] / 1000.0
    # passo todo dentro de áudio zerado = piso (-120 dB), como o RMS de zeros no worker real
    db = [-120.0 if fake_audible(p["mute"], i * step, (i + 1) * step) is None else level for i in range(n)]
    send({"type": "result", "id": ctx.id, "step_ms": p["step_ms"], "db": db})


# ---------------------------------------------------------------- modo real

_whisper = {"key": None, "model": None}


def looks_like_oom(e):
    s = str(e).lower()
    return isinstance(e, MemoryError) or "bad_alloc" in s or "out of memory" in s


def decode(path):
    """Áudio inteiro (FLAC/WAV...) em float32 mono 16 kHz, pelo mesmo decodificador do faster-whisper."""
    if not os.path.isfile(path):
        raise WorkerError("audio_decode", "file not found: %s" % os.path.basename(path))
    from faster_whisper import decode_audio
    try:
        return decode_audio(path, sampling_rate=SAMPLE_RATE)
    except MemoryError:
        raise
    except Exception as e:
        raise WorkerError("audio_decode", "%s: %s" % (type(e).__name__, e))


def apply_mute(audio, ranges):
    """Zera as amostras dentro dos cortes (segundos do arquivo), logo depois de decodificar e antes de qualquer
    processamento. Os tempos seguem os do arquivo: nada é fatiado nem concatenado (sem `clip_timestamps`). O VAD
    pula o silêncio resultante, então essas partes quase não custam tempo."""
    if not ranges:
        return audio
    flags = getattr(audio, "flags", None)
    if flags is not None and not flags.writeable:
        audio = audio.copy()
    n = len(audio)
    for a, b in ranges:
        i, j = max(0, int(round(a * SAMPLE_RATE))), min(n, int(round(b * SAMPLE_RATE)))
        if j > i:
            audio[i:j] = 0.0
    return audio


def load_whisper(model_dir, threads):
    key = (model_dir, threads)
    if _whisper["key"] == key:
        return _whisper["model"]
    if not os.path.isfile(os.path.join(model_dir, "model.bin")):
        raise WorkerError("model_missing", "whisper model not found: %s" % os.path.basename(model_dir.rstrip("/")))
    _whisper["key"] = _whisper["model"] = None  # libera o anterior antes de carregar outro
    from faster_whisper import WhisperModel
    t0 = time.perf_counter()
    model = WhisperModel(model_dir, device="cpu", compute_type="int8", cpu_threads=threads, local_files_only=True)
    log("whisper model loaded in %.1fs" % (time.perf_counter() - t0))
    _whisper["key"], _whisper["model"] = key, model
    return model


class ProgressPacer:
    """Progresso do ASR sem esperar o fim da janela de ~30 s.

    O faster-whisper só entrega segmentos ao fim de cada janela (e roda o VAD antes da 1ª), então sem isto a
    trilha fica em 0 % por dezenas de segundos. Enquanto a próxima janela não chega, uma thread estima a posição
    (curva que sobe rápido e satura em 90 % da janela, com tempo característico aprendido da janela anterior).
    É só estimativa: nunca passa do fim da janela, nunca decresce e o valor real (fim do último segmento) só
    é enviado se superar o que já foi mostrado. O 100 % exato sai em `finish`.
    """

    WINDOW_S = 30.0
    TICK_S = 1.0

    def __init__(self, rid, start_s, total):
        self.rid, self.total = rid, total
        self.lock = threading.Lock()
        self.stop = threading.Event()
        self.shown = self.anchor = min(start_s, total)  # último valor enviado / posição real conhecida
        self.t_anchor = time.monotonic()
        self.tau = 4.0  # s até ~90 % da janela; reaprendido a cada janela concluída
        self.thread = threading.Thread(target=self._run, daemon=True)

    def _emit(self, audio_s):  # chamar com o lock
        if audio_s > self.shown:
            self.shown = audio_s
            send({"type": "progress", "id": self.rid, "stage": "transcribe", "audio_s": round(audio_s, 3),
                  "total_s": self.total})

    def _run(self):
        while not self.stop.wait(self.TICK_S):
            with self.lock:
                if self.stop.is_set():  # `abort`/`finish` chegaram enquanto esperava o lock
                    break
                window = min(self.WINDOW_S, self.total - self.anchor)
                elapsed = time.monotonic() - self.t_anchor
                self._emit(self.anchor + 0.9 * window * (1.0 - math.exp(-elapsed / self.tau)))

    def start(self):
        self.thread.start()

    def real(self, end):
        """Fim de um segmento já decodificado (posição real no arquivo)."""
        with self.lock:
            now = time.monotonic()
            gap = now - self.t_anchor
            if gap >= 0.5:  # segmentos da mesma janela chegam colados: só um intervalo longo mede uma janela
                self.tau = min(max(gap / 2.3, 1.0), 20.0)
            self.anchor, self.t_anchor = max(self.anchor, min(end, self.total)), now
            self._emit(self.anchor)

    def finish(self):
        """Para a estimativa e fecha em 100 % (o fim do último segmento costuma ficar antes do fim do arquivo)."""
        self.stop.set()
        self.thread.join(timeout=5)
        with self.lock:
            self._emit(self.total)

    def abort(self):
        """Para a estimativa; ao voltar, a thread não envia mais nada (o lock espera um `_emit` em curso)."""
        self.stop.set()
        with self.lock:
            pass


def real_transcribe(ctx, p):
    rid, t0 = ctx.id, time.perf_counter()
    send({"type": "progress", "id": rid, "stage": "loading_model"})
    audio = apply_mute(decode(p["audio"]), p["mute"])
    total = len(audio) / SAMPLE_RATE
    start_s = p["start_s"]
    # retomada: o VAD ignora clip_timestamps, então fatiamos o array e somamos start_s aos tempos
    clip = audio[int(round(start_s * SAMPLE_RATE)):] if start_s > 0 else audio
    send({"type": "progress", "id": rid, "stage": "transcribe", "audio_s": min(start_s, total), "total_s": total})
    if len(clip) == 0:
        send({"type": "result", "id": rid, "segments": 0, "seconds": 0.0, "language": p["language"]})
        return
    model = load_whisper(p["model_dir"], p["threads"])
    if ctx.cancelled():
        return send_cancelled(rid, 0)
    pacer = ProgressPacer(rid, start_s, total)
    pacer.start()  # cobre o VAD (dentro de transcribe) e a 1ª janela
    try:
        segments, info = model.transcribe(
            clip, language=p["language"], beam_size=p["beam_size"], vad_filter=True,
            vad_parameters={"min_silence_duration_ms": p["vad_min_silence_ms"]},
            condition_on_previous_text=False, hotwords=p["hotwords"] or None,
            word_timestamps=p["word_timestamps"],
        )
        n = 0
        for s in segments:  # gerador: cada janela de ~30 s decodifica ao iterar
            text = s.text.strip()
            end = s.end + start_s
            if text:
                msg = {"type": "segment", "id": rid, "start": round(s.start + start_s, 3), "end": round(end, 3), "text": text}
                if s.words:
                    msg["words"] = [[round(w.start + start_s, 3), round(w.end + start_s, 3), w.word.strip()]
                                    for w in s.words if w.word.strip()]
                send(msg)
                n += 1
            pacer.real(end)
            if ctx.cancelled():
                pacer.abort()  # nada de `progress` depois do `cancelled`
                return send_cancelled(rid, n)
        pacer.finish()
    finally:
        pacer.abort()
    send({"type": "result", "id": rid, "segments": n, "seconds": round(time.perf_counter() - t0, 2),
          "language": info.language})


def unit(v):
    """Vetor normalizado em L2 (lista de floats); norma zero devolve zeros."""
    n = math.sqrt(sum(x * x for x in v))
    return [x / n for x in v] if n > 0 else [0.0] * len(v)


def cosine(a, b):
    return sum(x * y for x, y in zip(unit(a), unit(b)))


def pick_turns(turns, k=EMB_TURNS, min_s=EMB_MIN_TURN_S):
    """Amostra de um grupo para o centroide: até `k` dos turnos mais longos com >= `min_s` s; sem nenhum, o mais longo."""
    turns = sorted(turns, key=lambda t: (-(t[1] - t[0]), t[0]))
    long = [t for t in turns if t[1] - t[0] >= min_s]
    return long[:k] or turns[:1]


def merge_voices(speech, cents, merge_similarity=MERGE_SIMILARITY, min_speaker_s=MIN_SPEAKER_S, max_speakers=None):
    """Junta os grupos brutos do clustering que são a mesma voz. `speech[i]` = segundos de fala do grupo i,
    `cents[i]` = centroide (qualquer norma; usa o cosseno). Do maior para o menor (empate: índice menor), cada grupo
    junta ao mais parecido dos já mantidos se cos >= `merge_similarity` OU se tem < `min_speaker_s` de fala; senão vira
    pessoa própria (o maior grupo sempre fica). Centroides NÃO são recalculados na junção (o resultado depende pouco
    da ordem). Depois, com mais de `max_speakers` mantidos, o de menos fala (somando o que já absorveu) junta ao mais
    parecido dos outros, até caber.
    Devolve {"labels": rótulo final por grupo (0 = quem mais fala), "kept": nº final, "rows": [[fala, junta_em, cos]]
    na ordem decrescente de fala; `junta_em` = posição dessa lista do grupo que ficou com ele (None = é pessoa)}."""
    n = len(speech)
    order = sorted(range(n), key=lambda i: (-speech[i], i))
    parent = list(range(n))
    sim = [None] * n
    kept = []

    def best_of(i, pool):
        """(cosseno, grupo) do mais parecido de `pool`; empate: o primeiro (o de mais fala)."""
        best = None
        for k in pool:
            c = cosine(cents[i], cents[k])
            if best is None or c > best[0]:
                best = (c, k)
        return best

    for i in order:
        if kept:
            c, k = best_of(i, kept)
            sim[i] = c
            if c >= merge_similarity or speech[i] < min_speaker_s:
                parent[i] = k
                continue
        kept.append(i)

    def root(i):
        while parent[i] != i:
            i = parent[i]
        return i

    total = {k: 0.0 for k in kept}
    for i in range(n):
        total[root(i)] += speech[i]
    if max_speakers:
        while len(kept) > max_speakers:
            small = min(kept, key=lambda k: (total[k], -order.index(k)))
            pool = [k for k in kept if k != small]
            c, k = best_of(small, pool)
            sim[small], parent[small] = c, k
            total[k] += total.pop(small)
            kept = pool
    ranked = sorted(kept, key=lambda k: (-total[k], order.index(k)))
    label = {k: r for r, k in enumerate(ranked)}
    pos = {i: r for r, i in enumerate(order)}
    rows = [[speech[i], None if root(i) == i else pos[root(i)], sim[i]] for i in order]
    return {"labels": [label[root(i)] for i in range(n)], "kept": len(kept), "rows": rows}


def cluster_centroids(ctx, sherpa_onnx, p, audio, groups):
    """Centroide por grupo: média normalizada dos embeddings normalizados dos turnos amostrados (`pick_turns`, cada um
    cortado em `EMB_MAX_TURN_S` pelo meio). `groups` = lista de listas de (início, fim). None = cancelado."""
    import numpy as np
    rid = ctx.id
    picks = [pick_turns(g) for g in groups]
    total = sum(len(g) for g in picks)
    ex = sherpa_onnx.SpeakerEmbeddingExtractor(
        sherpa_onnx.SpeakerEmbeddingExtractorConfig(model=p["emb_model"], num_threads=max(1, p["threads"])))
    done, last, cents = 0, 0.0, []
    for g in picks:
        acc = None
        for a, b in g:
            if ctx.cancelled():
                return None
            mid, half = (a + b) / 2.0, min(b - a, EMB_MAX_TURN_S) / 2.0
            chunk = audio[max(0, int(round((mid - half) * SAMPLE_RATE))):int(round((mid + half) * SAMPLE_RATE))]
            if len(chunk):
                st = ex.create_stream()
                st.accept_waveform(sample_rate=SAMPLE_RATE, waveform=chunk)
                st.input_finished()
                if ex.is_ready(st):
                    e = np.asarray(ex.compute(st), dtype=np.float64)
                    n = np.linalg.norm(e)
                    if n > 0:
                        acc = e / n if acc is None else acc + e / n
            done += 1
            now = time.monotonic()
            if now - last >= 0.2 or done >= total:
                last = now
                send({"type": "progress", "id": rid, "stage": "diarize_embedding", "done": done, "total": total})
        cents.append(unit(acc.tolist()) if acc is not None else [])
    return cents


def real_diarize(ctx, p):
    rid = ctx.id
    for key in ("seg_model", "emb_model"):
        if not os.path.isfile(p[key]):
            raise WorkerError("model_missing", "diarization model not found: %s" % os.path.basename(p[key]))
    send({"type": "progress", "id": rid, "stage": "diarize_segmentation"})
    audio = apply_mute(decode(p["audio"]), p["mute"])
    if len(audio) == 0:
        send({"type": "result", "id": rid, "turns": [], "speakers": 0})
        return
    import sherpa_onnx
    config = sherpa_onnx.OfflineSpeakerDiarizationConfig(
        segmentation=sherpa_onnx.OfflineSpeakerSegmentationModelConfig(
            pyannote=sherpa_onnx.OfflineSpeakerSegmentationPyannoteModelConfig(
                model=p["seg_model"], window_shift_ratio=0.1),
            num_threads=max(1, p["threads"])),
        embedding=sherpa_onnx.SpeakerEmbeddingExtractorConfig(model=p["emb_model"], num_threads=max(1, p["threads"])),
        # nunca força a contagem: o limiar decide e `merge_voices` junta o que é a mesma voz (#25)
        clustering=sherpa_onnx.FastClusteringConfig(num_clusters=-1, threshold=p["threshold"]),
        min_duration_on=0.3, min_duration_off=0.5)
    if not config.validate():
        raise WorkerError("model_missing", "invalid diarization config (check model files)")
    sd = sherpa_onnx.OfflineSpeakerDiarization(config)
    state = {"last": 0.0, "prev_done": 0, "stage": "diarize_segmentation", "aborted": False}

    def on_progress(done, total):
        if ctx.cancelled():
            state["aborted"] = True
            return 1  # não-zero aborta o processamento
        if done < state["prev_done"]:  # o contador recomeça na fase de embeddings
            state["stage"] = "diarize_embedding"
        state["prev_done"] = done
        now = time.monotonic()
        if now - state["last"] >= 0.2 or done >= total:
            state["last"] = now
            send({"type": "progress", "id": rid, "stage": state["stage"], "done": int(done), "total": int(total)})
        return 0

    result = sd.process(audio, callback=on_progress)
    if state["aborted"] or ctx.cancelled():
        return send_cancelled(rid, 0)
    raw = [(round(r.start, 3), round(r.end, 3), int(r.speaker)) for r in result.sort_by_start_time()]
    ids = sorted({s for _, _, s in raw})
    groups = [[(a, b) for a, b, s in raw if s == c] for c in ids]
    speech = [round(sum(b - a for a, b in g), 3) for g in groups]
    if len(ids) > 1:
        cents = cluster_centroids(ctx, sherpa_onnx, p, audio, groups)
        if cents is None:
            return send_cancelled(rid, 0)
        merged = merge_voices(speech, cents, p["merge_similarity"], p["min_speaker_s"], p["max_speakers"])
    else:
        merged = {"labels": [0] * len(ids), "kept": len(ids), "rows": [[s, None, None] for s in speech]}
    label = dict(zip(ids, merged["labels"]))
    turns = [{"start": a, "end": b, "speaker": label[s]} for a, b, s in raw]
    rows = [[round(sp, 1), into, None if c is None else round(c, 3)] for sp, into, c in merged["rows"]]
    send({"type": "result", "id": rid, "turns": turns, "speakers": merged["kept"],
          "merge": {"raw": len(ids), "final": merged["kept"], "clusters": rows}})


def real_energy(ctx, p):
    import numpy as np
    audio = apply_mute(decode(p["audio"]), p["mute"])
    if ctx.cancelled():
        return send_cancelled(ctx.id, 0)
    step = SAMPLE_RATE * p["step_ms"] // 1000 or 1
    full = len(audio) // step
    ms = np.empty(0, dtype=np.float64)
    if full:
        blocks = audio[: full * step].reshape(full, step)
        ms = np.einsum("ij,ij->i", blocks, blocks, dtype=np.float64) / step
    if len(audio) > full * step:  # último passo parcial: RMS só das amostras que existem
        tail = audio[full * step:].astype(np.float64)
        ms = np.append(ms, float(np.dot(tail, tail)) / len(tail))
    db = np.round(10.0 * np.log10(np.maximum(ms, 1e-12)), 1)  # dBFS (RMS); piso -120
    send({"type": "result", "id": ctx.id, "step_ms": p["step_ms"], "db": db.tolist()})


HANDLERS = {
    False: {"transcribe": real_transcribe, "diarize": real_diarize, "energy": real_energy},
    True: {"transcribe": fake_transcribe, "diarize": fake_diarize, "energy": fake_energy},
}


# ---------------------------------------------------------------- laço principal

def run_request(msg, fake):
    rid = msg["id"]
    fatal_exit = False
    try:
        HANDLERS[fake][msg["type"]](Ctx(rid), parse_request(msg))
    except BadRequest as e:
        send_error(rid, "bad_request", e)
    except WorkerError as e:
        send_error(rid, e.code, e.detail, e.fatal)
        fatal_exit = e.fatal
    except Exception as e:
        log(traceback.format_exc())
        if looks_like_oom(e):
            send_error(rid, "oom", "%s: %s" % (type(e).__name__, e), fatal=True)
            fatal_exit = True
        else:
            send_error(rid, "exception", "%s: %s" % (type(e).__name__, e))
    finally:
        with STATE.lock:
            STATE.live.discard(rid)
            STATE.cancelled.discard(rid)
    return not fatal_exit


def reader(jobs):
    """Thread leitora do stdin: enfileira pedidos e aplica `cancel`/`shutdown` na hora."""
    stdin = sys.stdin.buffer
    while True:
        try:
            raw = stdin.readline()
        except (OSError, ValueError):
            raw = b""
        if not raw:
            break  # EOF: sem pai não há o que fazer
        if not raw.strip():
            continue
        try:
            msg = json.loads(raw.decode("utf-8"))
            if not isinstance(msg, dict):
                raise ValueError("not an object")
        except ValueError as e:  # inclui UnicodeDecodeError
            send_error(None, "bad_request", "invalid JSON line: %s" % e)
            continue
        kind, rid = msg.get("type"), msg.get("id")
        if kind == "cancel":
            with STATE.lock:
                if rid in STATE.live:  # cancel de pedido já terminado é ignorado
                    STATE.cancelled.add(rid)
            STATE.wake.set()
        elif kind == "shutdown":
            with STATE.lock:
                STATE.stopping = True
            STATE.wake.set()
            jobs.put(msg)
        elif kind in REQUESTS:
            if not isinstance(rid, str) or not rid:
                send_error(None, "bad_request", "missing or invalid id for %s" % kind)
                continue
            with STATE.lock:
                STATE.live.add(rid)
            jobs.put(msg)
        else:
            send_error(rid if isinstance(rid, str) else None, "bad_request", "unknown type: %r" % (kind,))
    os._exit(0)


def main(argv):
    fake = "--fake" in argv or bool(os.environ.get("TARY_FAKE_WORKER"))
    os.environ.setdefault("HF_HUB_OFFLINE", "1")  # sem rede: modelos sempre locais
    attach_protocol_stream()
    send(hello(fake))
    jobs = queue.Queue()
    threading.Thread(target=reader, args=(jobs,), daemon=True).start()
    code = 0
    while True:
        msg = jobs.get()
        if msg["type"] == "shutdown":
            send({"type": "bye"})
            break
        if not run_request(msg, fake):
            code = 1  # erro fatal já informado: o pai reinicia o worker
            break
    sys.stderr.flush()
    os._exit(code)  # a thread leitora (daemon) está bloqueada no stdin; evita travar no encerramento


if __name__ == "__main__":
    main(sys.argv[1:])
