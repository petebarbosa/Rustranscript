"""Testes do worker: `timeout 300 python3 -m unittest worker/test_worker.py -v`.
Falam com o processo de verdade, como o pai Rust: JSON-lines em stdin/stdout.

- Modo `--fake` (só stdlib): sempre roda.
- Teste REAL (opt-in): defina RSTT_WORKER_E2E=1, RSTT_E2E_PYTHON (python do venv com
  faster-whisper/sherpa-onnx), RSTT_E2E_AUDIO (clipe curto, <= 90 s, com fala) e
  RSTT_E2E_MODELS (pasta com `whisper-large-v3-turbo/` e `diar/` -> segmentação + embedding).
"""
import json
import math
import os
import queue
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
WORKER = os.path.join(HERE, "worker.py")


def write_flac_header(path, seconds, rate=16000):
    """FLAC mínimo: só `fLaC` + STREAMINFO (o worker falso lê apenas a duração; não há áudio)."""
    samples = int(round(seconds * rate))
    packed = (rate << 44) | (0 << 41) | (15 << 36) | samples  # 20 taxa | 3 canais-1 | 5 bps-1 | 36 amostras
    info = bytes(10) + packed.to_bytes(8, "big") + bytes(16)  # tamanhos de bloco/frame zerados + MD5 zerado
    with open(path, "wb") as f:
        f.write(b"fLaC" + bytes([0x80, 0, 0, 34]) + info)


class Worker:
    """Lado do pai: lê o stdout numa thread (fila) para poder esperar com prazo."""

    def __init__(self, *args, python=None, env=None):
        e = dict(os.environ)
        e.update(env or {})
        self.p = subprocess.Popen(
            [python or sys.executable, WORKER, *args], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True, bufsize=1, env=e,
        )
        self.q = queue.Queue()
        self.stderr = []
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=lambda: self.stderr.extend(self.p.stderr), daemon=True).start()

    def _read(self):
        for line in self.p.stdout:
            self.q.put((time.monotonic(), line.rstrip("\n")))
        self.q.put((time.monotonic(), None))  # EOF

    def send(self, msg):
        self.p.stdin.write((msg if isinstance(msg, str) else json.dumps(msg)) + "\n")
        self.p.stdin.flush()

    def recv_raw(self, timeout=30):
        return self.q.get(timeout=timeout)[1]

    def wait_eof(self, timeout=10):
        """Descarta o que já estava no pipe e espera o EOF (o pai vê o processo morrer)."""
        end = time.monotonic() + timeout
        while self.recv_raw(max(0.1, end - time.monotonic())) is not None:
            pass

    def recv(self, timeout=30):
        line = self.recv_raw(timeout)
        return json.loads(line) if line is not None else None

    def until(self, kinds=("result", "error", "cancelled"), timeout=60):
        """Mensagens até a terminal (inclusive)."""
        out = []
        while True:
            m = self.recv(timeout)
            out.append(m)
            if m is None or m["type"] in kinds:
                return out

    def close(self):
        try:
            self.p.stdin.close()
        except OSError:
            pass
        try:
            self.p.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.p.kill()
            self.p.wait()
        self.p.stdout.close()
        self.p.stderr.close()


class FakeBase(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.sys_flac = os.path.join(self.tmp.name, "sys.flac")
        self.mic_flac = os.path.join(self.tmp.name, "mic.flac")
        write_flac_header(self.sys_flac, 23.0)  # segmentos em 0, 5, 10, 15, 20
        write_flac_header(self.mic_flac, 23.0)
        self.start()

    def start(self, **env):
        self.w = Worker("--fake", env=env)
        self.addCleanup(self.w.close)
        self.hello = self.w.recv()

    def transcribe(self, rid="t1", track="sys", words=False, start_s=0.0, language=None, audio=None):
        return {"type": "transcribe", "id": rid, "audio": audio or self.sys_flac, "track": track,
                "model_dir": "/m", "language": language, "hotwords": None, "beam_size": 5, "threads": 1,
                "word_timestamps": words, "vad_min_silence_ms": 500, "start_s": start_s}


class FakeHandshake(FakeBase):
    def test_hello_is_first_line(self):
        h = self.hello
        self.assertEqual(h["type"], "hello")
        self.assertEqual(h["protocol"], 1)
        self.assertTrue(h["fake"])
        self.assertIsNone(h["faster_whisper"])
        self.assertIsNone(h["sherpa_onnx"])
        self.assertIsInstance(h["pid"], int)
        self.assertRegex(h["python"], r"^3\.\d+\.\d+$")

    def test_shutdown_says_bye_and_exits_zero(self):
        self.w.send({"type": "shutdown"})
        self.assertEqual(self.w.recv(), {"type": "bye"})
        self.assertEqual(self.w.p.wait(timeout=10), 0)

    def test_eof_exits_zero(self):
        self.w.p.stdin.close()
        self.assertEqual(self.w.p.wait(timeout=10), 0)

    def test_env_var_also_enables_fake(self):
        w = Worker(env={"RSTT_FAKE_WORKER": "1"})
        self.addCleanup(w.close)
        self.assertTrue(w.recv()["fake"])


class FakeTranscribe(FakeBase):
    def test_rule_sys(self):
        self.w.send(self.transcribe(words=True))
        msgs = self.w.until()
        segs = [m for m in msgs if m["type"] == "segment"]
        self.assertEqual([(s["start"], s["end"], s["text"]) for s in segs],
                         [(0.0, 4.5, "fala trecho 0"), (5.0, 9.5, "fala trecho 1"), (10.0, 14.5, "fala trecho 2"),
                          (15.0, 19.5, "fala trecho 3"), (20.0, 23.0, "fala trecho 4")])
        w = segs[4]["words"]  # [20, 23] em 3 partes iguais
        self.assertEqual([x[2] for x in w], ["fala", "trecho", "4"])
        self.assertEqual([x[0] for x in w], [20.0, 21.0, 22.0])
        self.assertEqual(w[2][1], 23.0)
        self.assertEqual(msgs[-1], {"type": "result", "id": "t1", "segments": 5, "seconds": 0.0, "language": "pt"})
        self.assertTrue(all(m["id"] == "t1" for m in msgs))
        # a primeira mensagem da etapa é o progresso de carga do modelo
        self.assertEqual((msgs[0]["type"], msgs[0]["stage"]), ("progress", "loading_model"))

    def test_mic_without_words_and_language(self):
        self.w.send(self.transcribe(track="mic", language="en", audio=self.mic_flac))
        msgs = self.w.until()
        segs = [m for m in msgs if m["type"] == "segment"]
        self.assertEqual(segs[1]["text"], "eu trecho 1")
        self.assertNotIn("words", segs[1])
        self.assertEqual(msgs[-1]["language"], "en")

    def test_start_s_resumes_without_repeating(self):
        self.w.send(self.transcribe(start_s=12.0))  # k = ceil(12/5) = 3
        msgs = self.w.until()
        texts = [m["text"] for m in msgs if m["type"] == "segment"]
        self.assertEqual(texts, ["fala trecho 3", "fala trecho 4"])
        self.assertEqual(msgs[-1]["segments"], 2)
        self.w.send(self.transcribe(rid="t2", start_s=15.0))  # fronteira exata: k = 3
        self.assertEqual([m["text"] for m in self.w.until() if m["type"] == "segment"], ["fala trecho 3", "fala trecho 4"])
        self.w.send(self.transcribe(rid="t3", start_s=100.0))
        self.assertEqual(self.w.until()[-1]["segments"], 0)

    def test_progress_total_is_file_duration(self):
        self.w.send(self.transcribe(start_s=10.0))
        prog = [m for m in self.w.until() if m["type"] == "progress" and m["stage"] == "transcribe"]
        self.assertEqual(prog[0]["audio_s"], 10.0)
        self.assertEqual({p["total_s"] for p in prog}, {23.0})
        self.assertEqual(prog[-1]["audio_s"], 23.0)


class FakeDiarizeEnergy(FakeBase):
    def diarize(self, rid="d1", n=None):
        return {"type": "diarize", "id": rid, "audio": self.sys_flac, "seg_model": "/s", "emb_model": "/e",
                "num_clusters": n, "threshold": 0.9, "threads": 1}

    def test_diarize_alternates_two_speakers(self):
        self.w.send(self.diarize())
        r = self.w.until()[-1]
        self.assertEqual(r["turns"], [{"start": 0.0, "end": 15.0, "speaker": 0}, {"start": 15.0, "end": 23.0, "speaker": 1}])
        self.assertEqual(r["speakers"], 2)

    def test_diarize_three_clusters(self):
        write_flac_header(self.sys_flac, 50.0)
        self.w.send(self.diarize(n=3))
        r = self.w.until()[-1]
        self.assertEqual([t["speaker"] for t in r["turns"]], [0, 1, 2, 0])
        self.assertEqual(r["speakers"], 3)

    def test_energy_levels_by_track(self):
        for name, path, level in (("e1", self.sys_flac, -20.0), ("e2", self.mic_flac, -25.0)):
            self.w.send({"type": "energy", "id": name, "audio": path, "step_ms": 100})
            r = self.w.until()[-1]
            self.assertEqual((r["type"], r["step_ms"], len(r["db"])), ("result", 100, 230))
            self.assertEqual(set(r["db"]), {level})
        write_flac_header(self.sys_flac, 1.01)
        self.w.send({"type": "energy", "id": "e3", "audio": self.sys_flac, "step_ms": 100})
        self.assertEqual(len(self.w.until()[-1]["db"]), 11)  # ceil(1010/100)


class FakeCancel(FakeBase):
    def start(self, **env):
        super().start(RSTT_FAKE_DELAY_MS="150", **env)

    def test_cancel_mid_transcribe(self):
        self.w.send(self.transcribe())
        while self.w.recv()["type"] != "segment":
            pass
        t0 = time.monotonic()
        self.w.send({"type": "cancel", "id": "t1"})
        last = self.w.until()[-1]
        self.assertEqual(last["type"], "cancelled")
        self.assertGreaterEqual(last["segments"], 1)
        self.assertLess(last["segments"], 5)
        self.assertLess(time.monotonic() - t0, 1.0)
        # o worker segue vivo e atende o próximo pedido
        self.w.send(self.transcribe(rid="t2", start_s=20.0))
        self.assertEqual(self.w.until()[-1]["segments"], 1)

    def test_cancel_before_first_segment_and_stale_cancel(self):
        self.w.send(self.transcribe())
        self.w.send({"type": "cancel", "id": "t1"})  # logo atrás do pedido
        self.assertEqual(self.w.until()[-1], {"type": "cancelled", "id": "t1", "segments": 0})
        # cancel de id inexistente/terminado é ignorado e não contamina o próximo uso do mesmo id
        self.w.send({"type": "cancel", "id": "t1"})
        self.w.send(self.transcribe(start_s=20.0))
        self.assertEqual(self.w.until()[-1]["type"], "result")

    def test_cancel_mid_diarize(self):
        write_flac_header(self.sys_flac, 300.0)  # 20 turnos x 150 ms
        self.w.send({"type": "diarize", "id": "d1", "audio": self.sys_flac, "seg_model": "/s", "emb_model": "/e",
                     "num_clusters": None, "threshold": 0.9, "threads": 1})
        while self.w.recv()["type"] != "progress":
            pass
        self.w.send({"type": "cancel", "id": "d1"})
        self.assertEqual(self.w.until()[-1]["type"], "cancelled")

    def test_shutdown_during_job_cancels_then_bye(self):
        self.w.send(self.transcribe())
        while self.w.recv()["type"] != "segment":
            pass
        self.w.send({"type": "shutdown"})
        msgs = self.w.until(kinds=("bye",))
        self.assertEqual([m["type"] for m in msgs if m["type"] in ("cancelled", "bye")], ["cancelled", "bye"])
        self.assertEqual(self.w.p.wait(timeout=10), 0)


class FakeErrors(FakeBase):
    def test_bad_json_and_unknown_type_do_not_kill_the_worker(self):
        self.w.send("isto não é json")
        e = self.w.recv()
        self.assertEqual((e["type"], e["code"], e["id"], e["fatal"]), ("error", "bad_request", None, False))
        self.w.send("[1, 2]")
        self.assertEqual(self.w.recv()["code"], "bad_request")
        self.w.send({"type": "nope", "id": "x"})
        e = self.w.recv()
        self.assertEqual((e["code"], e["id"]), ("bad_request", "x"))
        self.w.p.stdin.buffer.write(b"\xff\xfe\n")  # bytes inválidos em UTF-8
        self.w.p.stdin.flush()
        self.assertEqual(self.w.recv()["code"], "bad_request")
        self.w.send(self.transcribe(start_s=20.0))  # continua funcionando
        self.assertEqual(self.w.until()[-1]["type"], "result")

    def test_missing_and_invalid_fields(self):
        self.w.send({"type": "energy", "id": "e1", "step_ms": 100})
        e = self.w.recv()
        self.assertEqual((e["id"], e["code"]), ("e1", "bad_request"))
        self.assertIn("audio", e["detail"])
        self.w.send({"type": "energy", "id": "e2", "audio": self.sys_flac, "step_ms": "100"})
        self.assertEqual(self.w.recv()["code"], "bad_request")
        self.w.send({"type": "energy", "audio": self.sys_flac, "step_ms": 100})
        e = self.w.recv()
        self.assertEqual((e["id"], e["code"]), (None, "bad_request"))

    def test_audio_decode_error(self):
        self.w.send({"type": "energy", "id": "e1", "audio": "/nao/existe.flac", "step_ms": 100})
        e = self.w.recv()
        self.assertEqual((e["type"], e["id"], e["code"], e["fatal"]), ("error", "e1", "audio_decode", False))
        bad = os.path.join(self.tmp.name, "lixo.flac")
        with open(bad, "wb") as f:
            f.write(b"fLaC\x00")
        self.w.send({"type": "energy", "id": "e2", "audio": bad, "step_ms": 100})
        self.assertEqual(self.w.recv()["code"], "audio_decode")


class StdoutIsProtocolOnly(unittest.TestCase):
    def test_stray_prints_go_to_stderr(self):
        code = ("import sys; sys.path.insert(0, %r); import worker; worker.attach_protocol_stream();"
                "print('lixo de biblioteca'); import os; os.write(1, b'lixo nativo\\n');"
                "worker.send({'type': 'bye'})") % HERE
        r = subprocess.run([sys.executable, "-c", code], capture_output=True, text=True, timeout=30)
        self.assertEqual(r.stdout, '{"type":"bye"}\n')
        self.assertIn("lixo de biblioteca", r.stderr)
        self.assertIn("lixo nativo", r.stderr)


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    with open("/proc/%d/stat" % pid) as f:
        return f.read().split()[2] != "Z"


class NoOrphans(unittest.TestCase):
    alive = staticmethod(alive)

    def test_kill9_worker_gives_eof_and_no_leftovers(self):
        w = Worker("--fake")
        self.addCleanup(w.close)
        w.recv()
        pid = w.p.pid
        os.kill(pid, signal.SIGKILL)
        w.wait_eof()  # EOF no pai
        w.p.wait(timeout=10)
        self.assertFalse(self.alive(pid))

    def test_parent_kill9_worker_exits_by_itself(self):
        code = ("import subprocess, sys, time; p = subprocess.Popen([sys.executable, %r, '--fake'],"
                "stdin=subprocess.PIPE, stdout=subprocess.PIPE); print(p.pid, flush=True); time.sleep(60)") % WORKER
        parent = subprocess.Popen([sys.executable, "-c", code], stdout=subprocess.PIPE, text=True)
        self.addCleanup(parent.stdout.close)
        self.addCleanup(parent.wait)
        pid = int(parent.stdout.readline())
        self.assertTrue(self.alive(pid))
        parent.kill()  # SIGKILL no pai: o pipe do stdin do worker fecha
        parent.wait()
        deadline = time.monotonic() + 5
        while self.alive(pid) and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertFalse(self.alive(pid))


# ---------------------------------------------------------------- real (opt-in)

E2E = os.environ.get("RSTT_WORKER_E2E")


@unittest.skipUnless(E2E, "defina RSTT_WORKER_E2E=1 (+ _E2E_PYTHON/_E2E_AUDIO/_E2E_MODELS)")
class RealE2E(unittest.TestCase):
    alive = staticmethod(alive)

    @classmethod
    def setUpClass(cls):
        cls.python = os.environ["RSTT_E2E_PYTHON"]
        cls.audio = os.environ["RSTT_E2E_AUDIO"]
        models = os.environ["RSTT_E2E_MODELS"]
        cls.whisper = os.path.join(models, "whisper-large-v3-turbo")
        cls.seg = os.path.join(models, "diar", "sherpa-onnx-pyannote-segmentation-3-0", "model.onnx")
        cls.emb = os.path.join(models, "diar", "3dspeaker_speech_campplus_sv_zh_en_16k-common_advanced.onnx")
        cls.w = Worker(python=cls.python)
        cls.hello = cls.w.recv()
        cls.log = []

    @classmethod
    def tearDownClass(cls):
        cls.w.close()

    def show(self, msgs, limit=6):
        for m in msgs[:limit]:
            print("   ", json.dumps(m, ensure_ascii=False)[:230])

    def req(self, rid, start_s=0.0, words=True):
        return {"type": "transcribe", "id": rid, "audio": self.audio, "track": "sys", "model_dir": self.whisper,
                "language": "pt", "hotwords": "Kelvaris, Nortaflux, Brimodal", "beam_size": 5, "threads": 4,
                "word_timestamps": words, "vad_min_silence_ms": 500, "start_s": start_s}

    def test_1_hello(self):
        self.assertEqual(self.hello["protocol"], 1)
        self.assertFalse(self.hello["fake"])
        self.assertIsNotNone(self.hello["faster_whisper"])
        self.assertIsNotNone(self.hello["sherpa_onnx"])
        print("\n    hello:", json.dumps(self.hello))

    def test_2_energy(self):
        self.w.send({"type": "energy", "id": "en1", "audio": self.audio, "step_ms": 100})
        r = self.w.until(timeout=60)[-1]
        self.assertEqual(r["type"], "result")
        self.assertGreater(len(r["db"]), 100)
        self.assertTrue(all(isinstance(v, float) and -120.0 <= v <= 0.0 for v in r["db"]))
        self.assertGreater(max(r["db"]), -40.0)
        print("\n    energy: n=%d min=%.1f max=%.1f first=%s" % (len(r["db"]), min(r["db"]), max(r["db"]), r["db"][:5]))

    def test_3_transcribe_full_with_words_and_hotwords(self):
        self.w.send(self.req("tx1"))
        msgs = self.w.until(timeout=240)
        self.show([m for m in msgs if m["type"] != "progress"], 4)
        print("    ... result:", json.dumps(msgs[-1], ensure_ascii=False))
        segs = [m for m in msgs if m["type"] == "segment"]
        self.assertEqual(msgs[-1]["type"], "result")
        self.assertEqual(msgs[-1]["segments"], len(segs))
        self.assertGreater(len(segs), 3)
        self.assertTrue(all(s["words"] and s["start"] <= s["end"] for s in segs))
        self.assertTrue(all(a["end"] <= b["start"] + 0.5 for a, b in zip(segs, segs[1:])))
        RealE2E.full = segs
        RealE2E.total = [m for m in msgs if m["type"] == "progress" and m["stage"] == "transcribe"][0]["total_s"]
        print("    hotwords no texto:", [s["text"] for s in segs if any(h in s["text"] for h in ("Kelvaris", "Nortaflux", "Brimodal"))][:3])

    def test_4_resume_from_start_s_does_not_repeat(self):
        cut = RealE2E.full[len(RealE2E.full) // 2 - 1]["end"]  # retomada no último t_end gravado
        self.w.send(self.req("tx2", start_s=cut))
        msgs = self.w.until(timeout=240)
        segs = [m for m in msgs if m["type"] == "segment"]
        self.show([m for m in msgs if m["type"] != "progress"], 3)
        self.assertEqual(msgs[-1]["type"], "result")
        self.assertGreater(len(segs), 0)
        self.assertGreaterEqual(segs[0]["start"], cut - 0.01)
        first_prog = [m for m in msgs if m["type"] == "progress" and m["stage"] == "transcribe"][0]
        self.assertAlmostEqual(first_prog["audio_s"], cut, places=2)
        self.assertAlmostEqual(first_prog["total_s"], RealE2E.total, places=2)
        before = [s for s in RealE2E.full if s["end"] <= cut + 0.01]
        print("    start_s=%.2f: %d segmentos (1º início %.2f); completo tinha %d (%d antes do corte)"
              % (cut, len(segs), segs[0]["start"], len(RealE2E.full), len(before)))

    def test_5_cancel_mid_transcribe(self):
        self.w.send(self.req("tx3"))
        while self.w.recv(timeout=120)["type"] != "segment":
            pass
        t0 = time.monotonic()
        self.w.send({"type": "cancel", "id": "tx3"})
        last = self.w.until(timeout=60)[-1]
        dt = time.monotonic() - t0
        print("\n    cancel -> %s em %.1f s: %s" % (last["type"], dt, json.dumps(last)))
        self.assertEqual(last["type"], "cancelled")
        self.assertLess(dt, 30)  # ~uma janela de 30 s de áudio em decodificação (cancel é cooperativo)

    def test_6_diarize(self):
        self.w.send({"type": "diarize", "id": "dz1", "audio": self.audio, "seg_model": self.seg, "emb_model": self.emb,
                     "num_clusters": None, "threshold": 0.9, "threads": 4})
        msgs = self.w.until(timeout=200)
        r = msgs[-1]
        self.assertEqual(r["type"], "result")
        self.assertGreaterEqual(r["speakers"], 1)
        self.assertTrue(all(t["end"] > t["start"] for t in r["turns"]))
        prog = [m for m in msgs if m["type"] == "progress"]
        print("\n    diarize progress:", [json.dumps(p) for p in prog[:3]], "... n=%d" % len(prog))
        print("    result:", json.dumps({"turns": r["turns"][:3], "speakers": r["speakers"]}))
        self.w.send({"type": "diarize", "id": "dz2", "audio": self.audio, "seg_model": self.seg, "emb_model": self.emb,
                     "num_clusters": 2, "threshold": 0.9, "threads": 4})
        r2 = self.w.until(timeout=200)[-1]
        self.assertEqual(r2["type"], "result")
        print("    num_clusters=2 -> speakers=%d" % r2["speakers"])

    def test_7_errors_keep_worker_alive(self):
        self.w.send(dict(self.req("bad1"), audio="/nao/existe.flac"))
        self.assertEqual(self.w.until()[-1]["code"], "audio_decode")
        self.w.send(dict(self.req("bad2"), model_dir="/nao/existe"))
        e = self.w.until()[-1]
        self.assertEqual(e["code"], "model_missing")
        self.w.send({"type": "diarize", "id": "bad3", "audio": self.audio, "seg_model": "/x", "emb_model": "/y",
                     "num_clusters": None, "threshold": 0.9, "threads": 1})
        self.assertEqual(self.w.until()[-1]["code"], "model_missing")

    def test_8_kill9_leaves_no_orphan(self):
        w = Worker(python=self.python)
        w.recv()
        w.send(self.req("k1"))
        while w.recv(timeout=120)["type"] != "segment":
            pass
        pid = w.p.pid
        children = subprocess.run(["pgrep", "-P", str(pid)], capture_output=True, text=True).stdout.split()
        t0 = time.monotonic()
        os.kill(pid, signal.SIGKILL)
        w.wait_eof()
        w.p.wait(timeout=10)
        time.sleep(0.3)
        left = subprocess.run(["pgrep", "-f", "%s.*worker.py" % os.path.basename(self.python)], capture_output=True, text=True).stdout.split()
        print("\n    kill -9 -> EOF em %.0f ms; filhos antes: %s; processos restantes do pid: %s"
              % ((time.monotonic() - t0) * 1000, children, [x for x in left if int(x) == pid]))
        self.assertEqual(children, [])
        self.assertNotIn(str(pid), left)
        w.p.stdin.close()
        w.p.stdout.close()
        w.p.stderr.close()

    def test_9_parent_death_stops_busy_worker(self):
        code = ("import subprocess, sys, json, time; p = subprocess.Popen([%r, %r], stdin=subprocess.PIPE,"
                "stdout=subprocess.PIPE, text=True); print(p.pid, flush=True); p.stdout.readline();"
                "p.stdin.write(sys.argv[1] + '\\n'); p.stdin.flush(); p.stdout.readline(); time.sleep(60)") % (self.python, WORKER)
        parent = subprocess.Popen([sys.executable, "-c", code, json.dumps(self.req("p1"))], stdout=subprocess.PIPE, text=True)
        self.addCleanup(parent.stdout.close)
        pid = int(parent.stdout.readline())
        time.sleep(4)  # o worker já está no meio do pedido
        parent.kill()
        parent.wait()
        t0 = time.monotonic()
        while self.alive(pid) and time.monotonic() - t0 < 10:
            time.sleep(0.05)
        dt = time.monotonic() - t0
        print("\n    pai morto (kill -9) no meio do pedido -> worker saiu em %.0f ms" % (dt * 1000))
        self.assertLess(dt, 5)


if __name__ == "__main__":
    unittest.main()
