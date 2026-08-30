# 001 — Esqueleto del workspace y puerta de calidad

- **Fase:** 0
- **Estado:** propuesto

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
plugins y de frontend, y el núcleo no depende de la UI. Copiamos esa separación
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

- [ ] `cargo build --workspace --all-targets`
- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo fmt --all --check`
- [ ] `cargo run -p voltra-cli -- info` imprime versión, target y número de hilos.
- [ ] Tiempo de compilación limpia anotado aquí como línea base.

## Riesgos

- **`clippy::pedantic` es ruidoso.** Si bloquea más de lo que aporta, se
  permiten excepciones puntuales con `#[allow(...)]` **justificado por
  comentario**, nunca desactivándolo a nivel de workspace.
- **CI sin GPU ni dispositivos.** Se asume desde el principio: ninguna feature
  por defecto puede necesitarlos, y este plan no añade ninguna.
