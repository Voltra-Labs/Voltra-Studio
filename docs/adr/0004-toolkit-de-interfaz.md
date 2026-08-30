# ADR 0004 — Interfaz: `egui` / `eframe`

- **Estado:** aceptada (2026-08-30)
- **Fase:** 7

## Contexto

La UI de un estudio en directo tiene una exigencia poco común: la
previsualización es vídeo a 60 fps y los medidores de audio se refrescan a ritmo
de bloque, todo ello **sin robar tiempo al hilo de render**.

## Decisión

`egui` con `eframe`.

- Rust puro, sin dependencias del sistema más allá del windowing.
- Modo inmediato: la UI se redibuja desde un snapshot del estado, que es
  exactamente el modelo de concurrencia que impone CLAUDE.md §5 (la UI lee
  copias inmutables, nunca bloquea al motor).
- Integra con `wgpu`, el mismo backend del ADR 0001: la textura compuesta se
  puede mostrar en la previsualización **sin bajarla a CPU**.

## Alternativas descartadas

- **Tauri:** el webview añade IPC y latencia entre UI y motor, y obligaría a
  copiar cada frame de previsualización a través de esa frontera.
- **Slint:** buen rendimiento, pero sin la integración natural con `wgpu` ni el
  encaje con el modelo de snapshot.
- **Qt** (lo que usa OBS): C++ y binding frágil desde Rust; renunciamos a la
  familiaridad visual a cambio de un stack homogéneo.

## Consecuencias

- `voltra-ui` queda tras la feature `ui`; el CLI headless sigue siendo el banco
  de pruebas del motor y lo que corre en CI.
- El aspecto no será el de una app nativa; se asume y se compensa con un tema
  propio.
- La UI **no** puede tener acceso mutable directo al grafo de escena: edita una
  copia y la publica. Esto se hace cumplir con la API, no con disciplina.
