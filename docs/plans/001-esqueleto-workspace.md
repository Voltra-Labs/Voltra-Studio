# 001 — Esqueleto del workspace y puerta de calidad

- **Fase:** 0
- **Estado:** completado (2026-08-30)

## Objetivo

Dejar el repositorio con un workspace de Cargo que compila, se testea y pasa
`clippy -D warnings` y `fmt` en un contenedor headless, con los lints y perfiles
de compilación compartidos ya fijados. Al terminar, `cargo run -p voltra-cli --
info` imprime la versión y el entorno detectado.

## Fuera de alcance

- Cualquier tipo de dominio (frames, escenas, fuentes): eso es el plan 002.
- GPU, FFmpeg, captura y UI: no se añade ninguna dependencia nativa todavía.
- Crear las diez crates de CLAUDE.md §2 vacías. Se crean cuando se usan; el
  documento describe el destino, no el andamiaje.

## Referencia

De `docs/references/obs-studio.md`: libobs separa núcleo (`libobs`) de
plugins y de frontend, y el núcleo no depende de la UI. Adoptamos esa separación
desde el primer commit: `voltra-core` sin dependencias del sistema y
`voltra-cli` como frontend headless —el equivalente a tener un `obs-cli` desde
el día uno, que en OBS llegó tarde y por eso es incómodo automatizarlo.

## Diseño

Dos crates para empezar:

```
crates/
├── voltra-core/   # lib, sin deps del SO. Por ahora solo la versión y el error base.
└── voltra-cli/    # bin `voltra`. Subcomando `info`.
```

`Cargo.toml` del workspace fija:

- `resolver = "3"`, edición 2024, `rust-version` (MSRV) declarada.
- `[workspace.dependencies]` con las versiones centralizadas.
- `[workspace.lints]` con `clippy::all`, `clippy::pedantic`,
  `clippy::undocumented_unsafe_blocks`, `rust_2018_idioms`, `missing_docs`.
- Perfil `release` (`lto = "thin"`, `codegen-units = 1`) y perfil `profiling`
  (release + `debug = true`) para poder perfilar sin adivinar.

## Pasos

1. `Cargo.toml` del workspace + `rust-toolchain.toml`.
2. `voltra-core`: `lib.rs` con documentación de crate, `Error`/`Result` base y
   un test que fije la MSRV/versión.
3. `voltra-cli`: binario `voltra` con `clap`, subcomando `info` y `tracing`
   inicializado por `RUST_LOG`.
4. `.github/workflows/ci.yml`: fmt, clippy `-D warnings`, test, build release.
5. `deny.toml` para licencias y avisos de seguridad.
6. `README.md` con qué es Voltra, cómo compilar y cómo correr la puerta de
   calidad.

## Verificación

- [x] `cargo build --workspace --all-targets`
- [x] `cargo test --workspace` — 5 tests en verde
- [x] `cargo clippy --workspace --all-targets -- -D warnings`
- [x] `cargo fmt --all --check`
- [x] `cargo run -p voltra-cli -- info` imprime versión, MSRV, perfil, host y
      paralelismo disponible.
- [x] Línea base de compilación anotada abajo.

## Resultado

Línea base medida en el contenedor de desarrollo (x86_64-linux, 4 hilos):

| Medida | Valor |
|---|---|
| `cargo build --workspace --release` desde limpio | **19,4 s** |
| Binario `voltra` (release, `lto = "thin"`) | **1,81 MB** |
| Tests | 5 (4 en `voltra-core`, 1 en `voltra-cli`) |

Se vigilará el tiempo de compilación en los próximos pasos: es el primer aviso
de que una dependencia pesada se ha colado en las features por defecto.

## Desviaciones respecto al plan

- `clippy::unwrap_used` y `clippy::expect_used` saltaban dentro de los tests,
  donde CLAUDE.md §3 los permite expresamente. En vez de rebajar el lint —que es
  el que protege el código de librería— cada crate lleva
  `#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]`. Los
  tests de integración futuros en `tests/` necesitarán la misma línea.
- `clippy::pedantic` no dio guerra: no hizo falta ninguna excepción.
- `panic = "abort"` se descartó en el perfil `release`, en contra de lo que suele
  ponerse por defecto: el aislamiento de fuentes y plugins (CLAUDE.md §5, y la
  mejora que el ADR de extensiones perseguirá) depende del desenrollado de pila.
  Queda anotado como comentario en `Cargo.toml` para que nadie lo "arregle".
- Se añadió un trabajo de MSRV a CI, no previsto en el plan: declarar
  `rust-version` sin verificarlo es declarar una mentira.

## Riesgos

- **`clippy::pedantic` es ruidoso.** Si bloquea más de lo que aporta, se
  permiten excepciones puntuales con `#[allow(...)]` **justificado por
  comentario**, nunca desactivándolo a nivel de workspace.
- **CI sin GPU ni dispositivos.** Se asume desde el principio: ninguna feature
  por defecto puede necesitarlos, y este plan no añade ninguna.
