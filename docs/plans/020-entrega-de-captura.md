# 020 — De la captura al compositor: `voltra-capture` y su entrega

- **Fase:** 5
- **Estado:** en curso (2026-11-23). Se ejecuta sin esperar a que se revise el
  plan, porque el responsable lo ha pedido expresamente ("no me preguntes,
  dale").

## Objetivo

Nace la crate `voltra-capture` con lo que comparten todos los backends de
captura, antes de tener el primero: la entrega de fotogramas del hilo de
captura al de render por triple búfer (ADR 0006), con capturados, descartados,
enseñados y repetidos contados, y una `CaptureSource` que la escena trata como
cualquier otra fuente. Una captura de prueba en su propio hilo a 60 fps la
ejercita sin dispositivos, y un test mide lo que se pierde cuando los ritmos no
coinciden.

## Fuera de alcance

- **PipeWire**: es el plan 021. En este contenedor ya se ha comprobado que un
  PipeWire sin pantalla, con una fuente de vídeo de GStreamer, entrega 120
  fotogramas en 2,02 s (60 fps) a un consumidor enlazado a mano, así que podrá
  probarse aquí y en CI.
- El portal de captura de pantalla (`ashpd`), la cámara, el audio del sistema y
  la deriva entre relojes: los planes siguientes de la fase.
- Mapear el reloj de la captura al del motor: llega con el primer backend que
  trae una hora propia (021).
- El ritmo con búfer para cámaras que fluctúan, y la importación de DMA-BUF a la
  GPU.

## Referencia

`docs/references/capture.md`: las fuentes asíncronas de OBS, con una cola de
hasta 30 fotogramas bajo un mutex recursivo, que se vacía entera al llenarse;
su captura de pantalla por PipeWire, que se queda con el último búfer;
Windows.Graphics.Capture y ScreenCaptureKit, que entregan desde un *pool*
pequeño y fijo de búferes. Decisión: ADR 0006.

**Qué adoptamos:** para la captura, el último gana; el búfer del sistema se
devuelve cuanto antes.

**Qué mejoramos:** sin mutex ni espera entre hilos; el productor escribe en su
sitio, sin caché intermedia; cada descarte y cada repetición quedan contados;
ningún vaciado de cola entero.

## Diseño

### La entrega

```rust
pub fn frame_channel(format: PixelFormat, size: FrameSize) -> Result<(FrameWriter, FrameReader)>;

impl FrameWriter {                       // el hilo de captura
    pub fn frame_mut(&mut self, format: PixelFormat, size: FrameSize) -> Result<&mut VideoFrame>;
    pub fn publish(&mut self, pts: Timestamp);
    pub fn stats(&self) -> CaptureStats;
}

impl FrameReader {                       // el hilo de render
    pub fn update(&mut self) -> bool;    // ¿hay uno nuevo? cuenta enseñado o repetido
    pub fn frame(&self) -> Option<&VideoFrame>;  // None hasta el primero
    pub fn stats(&self) -> CaptureStats;
}

pub struct CaptureStats { pub captured: u64, pub dropped: u64, pub shown: u64, pub repeated: u64, pub resized: u64 }
```

- `TripleBuffer<VideoFrame>` de la crate `triple_buffer` 8.1 (ADR 0006).
- `frame_mut` devuelve el búfer del productor; si el formato o el tamaño no
  coinciden, lo reserva de nuevo y cuenta un cambio de tamaño. Lo que traiga
  dentro es basura: el productor lo sobrescribe entero.
- `publish` sella la hora, publica y, si el triple búfer avisa de que se
  sobrescribió uno sin leer, cuenta un descarte.
- Contadores en `AtomicU64` relajados y compartidos por los dos extremos.
- **Invariante**: `captured == shown + dropped + pendiente`, con `pendiente` 0 o
  1 (el publicado que aún no se ha leído).

### `CaptureSource`

```rust
pub trait Capture: Send + fmt::Debug {   // el backend, dueño de su hilo
    fn kind(&self) -> &'static str;
    fn status(&self) -> CaptureStatus;    // Starting, Running, Failed
}

pub struct CaptureSource { reader: FrameReader, backend: Box<dyn Capture> }
impl Source for CaptureSource { ... }      // tick → reader.update()
impl VideoSource for CaptureSource { ... } // frame → reader.frame()
```

Un backend que falla lo dice en su estado y deja de publicar; la fuente sigue
enseñando el último fotograma y el pipeline no se entera de nada más (§5).

### La captura de prueba

`TestPatternCapture::start(size, fps)`: un hilo que cada `1/fps`, medido con
plazos absolutos para que no derive, escribe una barra que se desplaza en BGRA
y publica. Se detiene y se espera al soltarla.

## Pasos

1. **Documentos**: referencia, ADR 0006 y este plan.
2. **La crate y la entrega**: `frame_channel`, los contadores, los tests (la
   invariante, que nunca se ve un fotograma más viejo después de uno más nuevo,
   y cero asignaciones al publicar y leer) y el benchmark.
3. **`CaptureSource` y la captura de prueba**, con el test de ritmos y los
   documentos.

## Verificación

- **Contadores:** tres publicados sin leer son dos descartes; leer sin publicar
  es una repetición; la invariante se cumple siempre, también con dos hilos
  publicando y leyendo a la vez durante miles de fotogramas.
- **Orden:** con dos hilos, el consumidor nunca ve una secuencia menor que la
  que ya vio.
- **Sin asignaciones:** `frame_mut` con el mismo tamaño, `publish` y `update` no
  reservan memoria, comprobado con un asignador que cuenta.
- **Ritmos**, con la captura de prueba a 60 fps durante 1 s: un consumidor a
  60 fps enseña casi todos; uno a 30 fps descarta casi la mitad. Las dos cosas
  se miden y se cumple la invariante. Los márgenes son anchos, porque el
  planificador de CI no es de tiempo real; lo exacto es la invariante.

**Rendimiento:** el coste de publicar y leer un fotograma 1080p (no depende del
tamaño: no hay copia) y de escribir el patrón, anotados en `PERFORMANCE.md`.

## Riesgos

- **`triple_buffer` exige `Clone` para crear los tres búferes**: `VideoFrame`
  lo es, y se clona solo al crear.
- **Los tests de ritmo dependen del planificador.** Por eso aseguran órdenes de
  magnitud y la invariante, no cifras exactas.
- **El modelo del último gana no sirve para todo**, como un vídeo de un fichero
  que hay que reproducir entero. No es captura, y tendrá otra entrega cuando
  exista.
