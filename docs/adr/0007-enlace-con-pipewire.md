# ADR 0007 — PipeWire por `pipewire-rs`, tras una feature

- **Estado:** aceptada (2026-11-23)
- **Fase:** 5

## Contexto

La captura de pantalla en Linux pasa por PipeWire (ADR 0002): el portal
`ScreenCast` entrega un descriptor y un nodo, y los fotogramas llegan por un
`pw_stream`. Es la primera dependencia nativa de la captura, y la heredarán el
audio del sistema y la cámara. Hay que elegir cómo hablar con ella.

## Opciones evaluadas

| Opción | A favor | En contra |
|---|---|---|
| **`pipewire-rs`** (la crate `pipewire` 0.10, de los propios mantenedores de PipeWire) | API segura sobre `libpipewire`: bucle, flujos, búferes con devolución al soltarlos, metadatos, `pw_time`; MIT; MSRV 1.80 | Bindings generados con `bindgen`: necesita `libpipewire-0.3-dev` y `libclang` al compilar |
| **`pipewire-sys` a mano** | Control total | Todo el `unsafe` lo escribiríamos nosotros, para reproducir lo que la crate ya da |
| **GStreamer (`pipewiresrc` por `gstreamer-rs`)** | Un elemento ya hecho | Arrastra GStreamer entero a la captura, con su propio modelo de hilos y de búferes entre nosotros y PipeWire |
| **El protocolo nativo en Rust puro** | Sin dependencia nativa | No hay una implementación madura; reescribir el cliente de PipeWire no es el trabajo de este proyecto |

## Decisión

**`pipewire` 0.10, tras la feature `pipewire` de `voltra-capture`**, apagada por
defecto. Es el mismo trato que `ffmpeg` en `voltra-encode`: la build por
defecto y `cargo test --workspace` no necesitan nada nativo; un trabajo de CI
propio instala `libpipewire-0.3-dev`, el demonio `pipewire` y GStreamer (solo
como **productor** de prueba) y ejecuta los tests de verdad.

## Consecuencias

- El backend corre su propio bucle de PipeWire en su propio hilo, como el
  `pw_thread_loop` de OBS; nada de PipeWire toca el hilo de render, que solo ve
  el triple búfer (ADR 0006).
- Los tests de la feature arrancan un `pipewire` sin pantalla ni D-Bus, publican
  una fuente de prueba con GStreamer y enlazan los puertos con `pw-link`: en
  este contenedor eso entrega 60 fps exactos (`docs/plans/021-captura-pipewire.md`).
- El portal (`ashpd`, MSRV 1.89) llegará en su propio plan, tras su propia
  feature: su dependencia es D-Bus, no PipeWire.
