# Estado del arte: de la captura al compositor

Referencia para la fase 5 y su primer paso, el plan 020. Una fuente de captura
—pantalla, ventana, cámara— produce fotogramas en **su** hilo y a **su** ritmo;
el compositor los consume en el suyo, al ritmo de la salida. Cómo se pasa un
fotograma de un hilo a otro, cuál se enseña cuando no coinciden los ritmos, y
qué se pierde por el camino, es lo que decide la latencia y los saltos de una
captura. Qué hace OBS, y cómo entregan sus fotogramas las APIs de Windows y
macOS con las que el trait tendrá que encajar (ADR 0002).

Fuentes (rama `master` de OBS, noviembre de 2026):

- [`libobs/obs-source.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-source.c) y [`libobs/obs-internal.h`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-internal.h) — las fuentes asíncronas.
- [`plugins/linux-pipewire/pipewire.c`](https://github.com/obsproject/obs-studio/blob/master/plugins/linux-pipewire/pipewire.c), [`screencast-portal.c`](https://github.com/obsproject/obs-studio/blob/master/plugins/linux-pipewire/screencast-portal.c) y [`formats.c`](https://github.com/obsproject/obs-studio/blob/master/plugins/linux-pipewire/formats.c) — la captura de pantalla en Linux.
- [`Direct3D11CaptureFramePool`](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframepool), [`Direct3D11CaptureFrame`](https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.direct3d11captureframe) y [la guía de captura](https://learn.microsoft.com/en-us/windows/uwp/audio-video-camera/screen-capture) — Windows.Graphics.Capture.
- [`SCStreamConfiguration.queueDepth`](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/queuedepth), [`minimumFrameInterval`](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/minimumframeinterval), [`SCStreamOutput`](https://developer.apple.com/documentation/screencapturekit/scstreamoutput) y [`SCFrameStatus`](https://developer.apple.com/documentation/screencapturekit/scframestatus) — ScreenCaptureKit.

---

## 1. OBS, fuentes asíncronas: una cola con mutex que se vacía entera

Una cámara o un medio empuja fotogramas con `obs_source_output_video`. Lo que
hace libobs con cada uno (`cache_video`, `obs_source_output_video_internal`):

1. **Lo copia** a un fotograma de su caché (`async_cache`, un *pool* sin límite
   explícito), bajo `async_mutex`, un mutex **recursivo**.
2. **Lo encola** en `async_frames`. Como mucho 30:

   ```c
   #define MAX_ASYNC_FRAMES 30
   ...
   if (source->async_frames.num >= MAX_ASYNC_FRAMES) {
       free_async_cache(source);
       source->last_frame_ts = 0;
       pthread_mutex_unlock(&source->async_mutex);
       return NULL;
   }
   ```

   Con la cola llena **no descarta el más viejo: descarta todos** y rechaza el
   que llega. Un cambio de formato o tamaño hace lo mismo.

En cada tick de vídeo, `async_tick` elige qué fotograma enseñar
(`get_closest_frame`, `ready_async_frame`). El reloj de la fuente
(`last_frame_ts`) avanza lo que avanzó la salida, y se consumen los fotogramas
cuya hora ya pasó:

```c
source->last_frame_ts += sys_offset;
while (source->last_frame_ts > next_frame->timestamp) {
    if (frame && (source->last_frame_ts - next_frame->timestamp) < 2000000)
        break;
```

Los que se saltan se borran de la cola. Un salto de más de `MAX_TS_VAR`
(2 s) en las marcas reancla el reloj. Si no hay fotograma nuevo, se queda en
pantalla la textura anterior.

Con `obs_source_set_async_unbuffered(true)` el ritmo desaparece: en cada tick
se descarta todo menos **el más reciente**, que se enseña.

**Qué no hay:** ningún contador de fotogramas descartados en la fuente. Se
pierden en tres sitios —cola llena, fotogramas que se quedaron atrás, modo sin
búfer— y ninguno queda medido. (Hay contadores de retraso de la mezcla de
vídeo, `lagged_frames`, pero son de la salida, no de la fuente.)

## 2. OBS en Linux: la captura de pantalla no es asíncrona

La captura de pantalla por PipeWire **no usa ese camino**. Es una fuente
síncrona (`OBS_SOURCE_VIDEO`, sin `OBS_SOURCE_ASYNC`) que se dibuja en
`video_render` con la última textura que recibió.

**Portal** (`screencast-portal.c`): `CreateSession` → `SelectSources` (tipos
`MONITOR = 1`, `WINDOW = 2`, `VIRTUAL = 4`; `multiple = false`; cursor en modo
*metadata* si el portal lo ofrece, si no incrustado u oculto) → `Start` →
`OpenPipeWireRemote`, que devuelve un descriptor. Con la versión 4 del portal
pide `persist_mode = 2` y guarda el `restore_token` en la configuración de la
fuente, para no volver a preguntar al usuario.

**PipeWire** (`pipewire.c`): un `pw_thread_loop` propio, `pw_context_connect_fd`
con el descriptor del portal, y un `pw_stream` de entrada con
`PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS`.

**Formatos** ofrecidos: `BGRA`, `RGBA`, `BGRx`, `RGBx` y, con PipeWire ≥ 0.3.41,
los de 10 bits `ABGR_210LE`/`xBGR_210LE`. Primero con modificadores DMA-BUF
—los que la GPU dice importar—, después sin ellos, como respaldo en memoria
compartida. Si importar un DMA-BUF falla, quita ese modificador y renegocia.

**Proceso** (`on_process_cb` → `process_video_sync`): **vacía la cola y se
queda con el último búfer**, devolviendo los anteriores sin usarlos:

```c
while (true) {
    struct pw_buffer *aux = pw_stream_dequeue_buffer(stream);
    if (!aux) break;
    if (b) return_unused_pw_buffer(stream, b);
    b = aux;
}
```

Un DMA-BUF se importa como textura (`gs_texture_create_from_dmabuf`) sin pasar
por la CPU; la memoria compartida se sube con `gs_texture_create` en cada
fotograma. Los búferes marcados `SPA_META_HEADER_FLAG_CORRUPTED` se descartan.
Se piden los metadatos de recorte (`VideoCrop`), cursor —que se dibuja aparte—,
cabecera y transformación; con PipeWire ≥ 1.2, sincronización explícita
(`SyncTimeline`). **No se usa la marca de tiempo de PipeWire** (`pts` de
`spa_meta_header`) ni hay contador de descartes.

## 3. Windows.Graphics.Capture

- Los fotogramas viven en un `Direct3D11CaptureFramePool` de `numberOfBuffers`
  superficies de GPU. Con `CreateFreeThreaded`, el evento `FrameArrived` llega
  en un hilo interno del *pool*.
- `TryGetNextFrame` saca un fotograma —o nada, si el *pool* está vacío—, que
  vuelve al *pool* al liberarlo: "Applications should not save references to
  Direct3D11CaptureFrame objects".
- Cada fotograma trae `SystemRelativeTime`, "the QPC time at which the
  compositor rendered the frame", su `ContentSize` y la `Surface`.
- Un cambio de tamaño exige `Recreate`, y "all existing frames are discarded".

## 4. ScreenCaptureKit

- Los fotogramas llegan como `CMSampleBuffer` a
  `stream(_:didOutputSampleBuffer:of:)`, en la cola de despacho que se pasa a
  `addStreamOutput`.
- `queueDepth`: "the maximum number of frames for the queue to store"; por
  defecto 3, el mínimo; "don't exceed a queue depth of eight frames".
- `minimumFrameInterval`: el intervalo mínimo entre fotogramas (`1/60` para
  60 fps); 0 es el máximo que admita el sistema.
- `SCFrameStatus` dice si el fotograma es nuevo (`complete`) o no lo hay porque
  la pantalla no cambió (`idle`), está en blanco, en pausa o parada.

## 5. Lo común

Las tres plataformas entregan igual: **un hilo del sistema avisa de que hay un
fotograma, que vive en un *pool* pequeño y fijo de búferes** (WGC
`numberOfBuffers`, SCK `queueDepth` de 3 a 8, PipeWire los búferes del
`pw_stream`), y que hay que devolver pronto. Retenerlo bloquea al productor. Las
tres traen la hora del fotograma en el reloj monótono del sistema (QPC,
`CLOCK_MONOTONIC` en PipeWire) y las tres pueden cambiar de tamaño durante la
captura. Y ninguna de ellas, ni OBS encima, cuenta lo que se pierde.

---

## 6. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:**

- **Para la pantalla, gana el último.** Es lo que hace OBS en Linux con su
  vaciado de cola y lo que hace en modo sin búfer: un fotograma de captura que
  llega tarde no tiene valor, y retenerlo solo añade latencia.
- Devolver el búfer del sistema cuanto antes: copiarlo o importarlo, y soltarlo.
- La hora del fotograma en el reloj monótono del sistema, y reanclar el reloj
  tras un salto grande (`MAX_TS_VAR`, 2 s).

**Qué hacemos distinto:**

1. **Sin mutex entre la captura y el compositor**: un triple búfer sin espera
   —el productor escribe en uno, el consumidor lee otro, y el tercero se
   intercambia con un atómico—, en vez de una cola bajo un mutex recursivo.
   Ninguno de los dos hilos espera nunca al otro.
2. **Escribir en su sitio**: el productor escribe en el búfer que le toca, que
   vuelve a él tras leerse; sin una copia a una caché intermedia ni una reserva
   por fotograma.
3. **Cada pérdida contada.** El triple búfer sabe si un fotograma se sobrescribe
   antes de leerse: eso es un descarte, y queda contado. Y un tick del
   compositor sin fotograma nuevo es una repetición, también contada. Con eso
   se cumple una invariante que un test puede exigir: capturados = enseñados +
   descartados (+ el que espera).
4. **Ningún vaciado de cola entero**: con un triple búfer no hay cola que se
   llene; un productor más rápido que el consumidor descarta de uno en uno, y
   lo dice.

**Qué queda fuera de esta referencia:** el ritmo con búfer para cámaras con
fluctuación (el modo por defecto de OBS para fuentes asíncronas), que llegará
cuando haya una cámara que lo necesite; y la importación de DMA-BUF a la GPU,
que necesita el backend `wgpu` y una GPU.
