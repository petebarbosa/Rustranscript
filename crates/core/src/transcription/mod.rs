//! Transcrição e separação de falantes (fase 4). Contrato: TRANSCRIPTION_CONTRACT.md (local).
//!
//! Camadas (de baixo para cima): `protocol` (mensagens do worker Python) → `engine` (quem executa um pedido:
//! `FakeEngine` em memória, `ProcessEngine` com o worker real) → `staging`/`assemble`/`commit` (bruto no banco,
//! montagem pura, versão numa transação) → `runner` (uma tarefa de ponta a ponta) → `queue` (tabela de tarefas
//! em `app.db`). `runtime` e `models` instalam o Python fixado e os modelos. A fila é da GUI (thread no shell);
//! o núcleo não cria threads de agendamento.
pub mod assemble;
pub mod commit;
pub mod engine;
pub mod keys;
pub mod models;
pub mod params;
pub mod protocol;
pub mod queue;
pub mod runner;
pub mod runtime;
pub mod staging;

/// Variável de ambiente de teste: `1` = shell usa o `ProcessEngine` com `worker.py --fake` (Python do
/// sistema, sem runtime nem modelos; instantâneo); `slow` = idem com 300 ms entre segmentos (testes de
/// cancelar/matar). Valor que é um caminho existente = esse interpretador em vez de `python3`.
pub const FAKE_WORKER_ENV: &str = "TRANSCRICOES_FAKE_WORKER";
