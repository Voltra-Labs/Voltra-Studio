# 021 — Captura de vídeo por PipeWire

- **Fase:** 5
- **Estado:** completado (2026-11-23) — ver Resultado. Se ejecutó sin esperar
  a que se revisara el plan, porque el responsable lo pidió expresamente ("no
  me preguntes, dale").

## Objetivo

`PipeWireCapture`, tras la feature `pipewire` de `voltra-capture`, recibe
fotogramas de un nodo de vídeo de PipeWire en su propio hilo y los publica en
la entrega del plan 020: una `CaptureSource` más. Un test arranca un PipeWire
sin pantalla con una fuente de 60 fps y mide lo que llega, lo que se descarta
en cada eslabón y con qué marca de tiempo.

## Fuera de alcance

- **El portal** `ScreenCast` (CreateSession, SelectSources, Start,
  OpenPipeWireRemote): necesita un escritorio y D-Bus, y su crate pide Rust 1.89.
  Este plan conecta a un nodo por su id o se deja enlazar.
- **DMA-BUF**: importar sin copia a la GPU necesita el backend `wgpu` y una GPU.
  Aquí solo memoria compartida, con la copia que eso implica.
- Formatos de 10 bits y YUV; el cursor como metadato; el recorte (`VideoCrop`).
- Grabar la captura en tiempo real con `voltra record`: necesita un bucle de
  render a ritmo de reloj, que es otro paso.

## Referencia

`docs/references/capture.md` §2, lo que hace OBS en `plugins/linux-pipewire`:

- Un bucle de PipeWire propio, y un `pw_stream` de entrada con
  `AUTOCONNECT | MAP_BUFFERS`.
- Ofrece `BGRA`, `RGBA`, `BGRx` y `RGBx`.
- En cada `process` vacía la cola y se queda con el último búfer.
- Descarta los búferes marcados `CORRUPTED`.
- No usa la marca de tiempo de PipeWire ni cuenta descartes.

Y el [tutorial 5 de PipeWire](https://docs.pipewire.org/page_tutorial5.html),
del que sale el ejemplo `streams.rs` de `pipewire-rs`. La dependencia se decide
en el ADR 0007.

**Qué adoptamos:**

- El bucle propio y los mismos cuatro formatos de 8 bits.
- Vaciar hasta el último búfer.
- Saltar los búferes corruptos y los vacíos.

**Qué mejoramos:**

- **Lo que se vacía se cuenta**: es un descarte en la fuente, antes de llegar
  a nosotros.
- **La marca de tiempo del fotograma** (`spa_meta_header.pts`), llevada al
  reloj monótono del proceso, en vez de la hora de llegada.
- **La copia** del búfer compartido a nuestro fotograma es la única, y va fila
  a fila respetando el `stride` de PipeWire.

## Diseño

### `PipeWireCapture`

```rust
pub struct PipeWireTarget { pub node: Option<u32>, pub autoconnect: bool, pub name: String }

impl PipeWireCapture {
    pub fn start(target: PipeWireTarget) -> Result<CaptureSource>;
}
```

- **Hilo:** el backend crea un hilo que inicializa PipeWire, crea su
  `MainLoop`, `Context`, `Core` y el `Stream`, y ejecuta el bucle. `stop` envía
  un mensaje por un `pipewire::channel` adjunto al bucle, que lo para; después
  se espera al hilo.
- **Negociación:** `EnumFormat` con vídeo crudo `BGRx`, `BGRA`, `RGBx` y
  `RGBA`, tamaño de 1×1 a 8192×8192 y cualquier ritmo. En `param_changed` con
  el formato elegido se guardan el formato y el tamaño.
- **`process`:**
  1. Se vacía la cola hasta el último búfer; los anteriores se devuelven y se
     cuentan como `source_dropped`.
  2. Se salta el búfer si su cabecera o su `chunk` dicen `CORRUPTED`, o si
     viene vacío.
  3. Se copian las filas a un fotograma `Bgra8` de `frame_mut`. `BGRx` y `BGRA`
     van tal cual, con el alfa a 255 en `x`. `RGBx` y `RGBA` intercambian rojo y
     azul.
  4. Se publica con su marca de tiempo.
- **Marca de tiempo:** si el búfer trae `spa_meta_header`, su `pts` está en
  `CLOCK_MONOTONIC`. Se lleva al reloj del proceso (un `Instant` de
  referencia) con la diferencia entre `pw_time.now` y un `Instant::now()`
  tomados juntos. Si no la trae, cuenta la hora de llegada.
- **Estado:** `Starting` hasta el primer fotograma, `Running`, y `Failed` si el
  flujo pasa a error o se desconecta; el motivo va al log.
- **Contadores propios:** además de los de la entrega, `source_dropped` y
  `skipped` (corruptos o vacíos).

### Tests, tras la feature

`tests/pipewire.rs` levanta lo que necesita, o se salta y lo dice si faltan
`pipewire`, `gst-launch-1.0` o `pw-link`:

1. **El servidor:** un `pipewire` con `XDG_RUNTIME_DIR` en un directorio corto
   y propio (la ruta del socket no puede pasar de 108 bytes).
2. **El productor:** una fuente de GStreamer, `videotestsrc is-live=true`, en
   `BGRx` a 320×180 y 60 fps, por `pipewiresink`.
3. **La conexión:** el backend con `autoconnect: false`, y los puertos
   enlazados con `pw-link`.

Durante 2 s se comprueba:

- llegan 120 ± 10 fotogramas, con el tamaño y el formato del productor;
- la invariante de la entrega se cumple;
- las marcas de tiempo avanzan alrededor de 16,7 ms entre fotogramas;
- al parar, el estado pasa a `Stopped`.

### CI

El trabajo `capture` instala `libpipewire-0.3-dev`, `clang`, `pipewire` y
`gstreamer1.0-pipewire`, más los plugins base y las herramientas de GStreamer,
y ejecuta clippy y los tests de `voltra-capture` con `--features pipewire`.

## Pasos

1. **Documentos**: este plan y el ADR 0007.
2. **El backend** tras la feature, con sus tests unitarios (la conversión de
   formatos, el mapeo de la marca de tiempo).
3. **El test de extremo a extremo** con PipeWire, el trabajo de CI y los
   documentos con las cifras.

## Verificación

- La conversión de cada formato da los bytes BGRA esperados, con `stride`
  mayor que el ancho.
- El mapeo de la marca de tiempo es exacto con relojes de prueba y reancla si
  la marca salta más de 2 s.
- De extremo a extremo, las cifras anteriores, en este contenedor y en CI.
- La build por defecto no cambia: sin la feature no se compila nada de
  PipeWire.

**Rendimiento:** el coste de copiar y convertir un fotograma 1080p desde
memoria compartida, anotado; es la deuda que DMA-BUF quitará.

## Riesgos

- **`pipewire-rs` y sus `bindgen`**: compilan aquí (comprobado), y CI instala
  las mismas cabeceras.
- **Un PipeWire en CI** puede no arrancar en el runner. Entonces el test se
  salta y lo dice, y la causa se anota; la build y clippy siguen comprobándose.
- **Enlazar con `pw-link`** es cosa del test. En un escritorio enlaza el gestor
  de sesión, o el portal entrega el nodo ya en su propio *remote*.

## Resultado

Puerta de calidad en verde: **357 tests** por defecto (desde 353) y el de
extremo a extremo con `--features pipewire`; clippy limpio con Rust 1.97 y
1.100, con y sin la feature; `cargo +1.85 check` y `cargo deny` bien. Tres
commits: documentos; el backend; el test de extremo a extremo, el trabajo de CI
y estos documentos.

**Funcionó a la primera contra un PipeWire de verdad.** Una fuente de GStreamer
a 60 fps, 2 s, cinco ejecuciones:

| Medida | Resultado |
|---|---|
| Capturados | 122 |
| Enseñados / descartados en la entrega | 118–120 / 1–3 |
| Descartados en la cola de PipeWire / saltados | 0 / 0 |
| Separación de las marcas del productor | 16,67 ms, 60 fps exactos, cuando no se descarta ninguno |
| Anclajes del reloj | 1 |
| Copiar 1080p a BGRA, según el formato | 561 µs (`BGRA`), 732 µs (`BGRx`), 1,32 ms (`RGBx`) |

## Desviaciones respecto al plan

- **`PipeWireTarget::remote`**, que el diseño no traía: la ruta absoluta del
  socket del servidor. Así el test conecta a su propio servidor sin tocar las
  variables de entorno del proceso, que en la edición 2024 es `unsafe`.
- **La conversión es pública** (`copy_to_bgra`, `SourceFormat`): la necesitarán
  los demás backends, y así tiene benchmark.
- **`Capture::backend_stats`**, con `source_dropped`, `skipped` y
  `clock_anchors`: lo que el backend cuenta antes de la entrega, visible desde
  la fuente.
- **Ni WirePlumber ni D-Bus en el test**: un `pipewire` solo basta cuando los
  puertos se enlazan con `pw-link`. Con WirePlumber, además, el enlace
  automático no funciona con una fuente de GStreamer ("target not found",
  aunque la registra como fuente de vídeo por defecto); con un portal, el nodo
  llega ya preparado.
- **El trabajo de la GPU (lavapipe) cayó con un SIGSEGV** en los tests de
  paridad, primero en la PR del plan 020 y otra vez en la de este, sin que
  ninguna tocara `voltra-render`. Al volver a ocurrir se investigó:
  - **Dónde:** CI ya corre en Ubuntu 26.04 con Mesa 26.0.8; este contenedor,
    con Mesa 24, pasa siempre.
  - **Causa probable:** cada test abría y destruía su propia instancia de
    Vulkan y su dispositivo, en hilos paralelos, a pesar de que `GpuContext`
    documenta que un proceso quiere un solo dispositivo.
  - **Arreglo:** los tests comparten ahora uno solo, que se abre la primera vez
    y no se cierra. Es lo que hará el programa, y además tardan un tercio
    (0,2 s frente a 0,6 s).
  - **Si vuelve a caer**, la causa es otra: el paso siguiente es sacar la pila
    con `gdb` en CI.

## Pasos futuros anotados

- El portal `ScreenCast` (`ashpd`, tras su propia feature), con el
  `restore_token` guardado como OBS.
- DMA-BUF a la GPU (`PERFORMANCE.md` §3.7).
- Un bucle de render a ritmo de reloj, para grabar una captura con
  `voltra record`.
