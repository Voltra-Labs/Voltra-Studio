# 016 — Grabación a Matroska

- **Fase:** 4
- **Estado:** en curso (2026-11-22) — ejecutado sin esperar revisión del plan,
  por indicación expresa del responsable ("no me preguntes, dale")

## Objetivo

Un muxer Matroska nativo en `voltra-output` y una orden `voltra record` que
graba la escena de demostración con un tono mezclado por el mezclador de la
fase 3: **vídeo y audio en un mismo fichero, reproducible, sincronizado y válido
hasta el último bloque aunque se corte**. Sin encoders todavía —vídeo I420 sin
comprimir, audio PCM float—, porque lo que este paso fija es lo que ningún
encoder cambia: el contenedor, las marcas de tiempo y el entrelazado.

## Fuera de alcance

- **Encoders.** H.264/AV1 por hardware o software (ADR 0003) son el plan
  siguiente, y entregarán sus paquetes a este muxer.
- **`Cues`** (índice para buscar rápido) y el tamaño final del `Segment`. El
  fichero se escribe en flujo; un índice al cerrar es una mejora aparte.
- **MP4**, por remuxeo o muxer propio (ADR 0005).
- **Grabar en tiempo real** desde un reloj de pared: esto renderiza tan rápido
  como puede, con el reloj virtual de los índices. El tiempo real llega con el
  motor y la captura.
- **Varias pistas de audio**, capítulos, metadatos.

## Referencia

`docs/references/recording.md` (OBS: muxer en proceso aparte, MP4 híbrido;
Matroska y EBML) y `docs/adr/0005-contenedor-de-grabacion.md`.

**Qué adoptamos:** el objetivo del MP4 híbrido de OBS —el fichero vale en todo
momento— y su decisión de escribir el muxer propio en vez de envolver
`libavformat`.

**Qué mejoramos:** Matroska en flujo, con `Segment` y `Cluster` de tamaño
desconocido, es válido en cada byte por la forma del formato, sin escribir
fragmentado y reescribir al final; sin dependencias nativas, así que se prueba
en CI; y marcas de tiempo desde el índice absoluto.

## Diseño

### El muxer: `voltra_output::matroska`

```rust
pub enum TrackSpec {
    RawVideo { size: FrameSize, fps: Fps },                 // V_UNCOMPRESSED, I420
    PcmAudio { rate: SampleRate, layout: ChannelLayout },    // A_PCM/FLOAT/IEEE
}

pub struct MatroskaWriter<W: Write> { ... }

impl<W: Write> MatroskaWriter<W> {
    pub fn new(sink: W, tracks: &[TrackSpec]) -> Result<Self>;
    pub fn write_block(&mut self, track: usize, pts: Timestamp, keyframe: bool,
                       payload: &[u8]) -> Result<()>;
    pub fn write_video(&mut self, track: usize, frame: &VideoFrame, pts: Timestamp) -> Result<()>;
    pub fn write_audio(&mut self, track: usize, block: &AudioBuffer) -> Result<()>;
    pub fn finish(self) -> Result<W>;
}
impl<W: Write + Seek> MatroskaWriter<W> {
    pub fn finish_with_duration(self) -> Result<W>;
}
```

- **Cabecera**, una vez: EBML (`DocType` `matroska`), `Segment` de tamaño
  desconocido, `Info` (`TimestampScale` 1 ms, `MuxingApp`, `WritingApp` y un
  `Duration` reservado) y `Tracks`.
- **`write_block`** es la primitiva que usarán los encoders: un `SimpleBlock`
  con número de pista, marca relativa al `Cluster` y los bytes del paquete.
  Escribe la cabecera del bloque y luego el payload tal cual, sin copiarlo.
- **`Cluster` de tamaño desconocido**, cortado cuando el bloque está a 1 s o más
  del inicio del actual (o más cerca del límite de 32,767 s que da el entero de
  16 bits).
- **Orden:** los bloques deben llegar en marca de tiempo no decreciente entre
  todas las pistas; si no, `Error::OutOfOrder`. Entrelazar es de quien escribe,
  que sabe qué viene después.
- **Marcas de tiempo:** nanosegundos a milisegundos redondeando al más cercano,
  desde la marca absoluta de cada bloque: el error de cada bloque es ≤ 0,5 ms y
  no se acumula. El milisegundo es la unidad que usa FFmpeg y la que todo
  reproductor espera; se pierde precisión de marca, no de muestra, porque el
  audio PCM lleva su propia duración en sus muestras.
- **`write_video`** escribe los tres planos I420 fila a fila, sin el relleno del
  `stride`, directamente al destino. **`write_audio`** convierte a PCM float
  intercalado con la conversión de borde del plan 011 en un búfer reutilizado:
  tras el primer bloque, no asigna.
- **`finish_with_duration`**, cuando el destino admite volver atrás, escribe la
  duración en el hueco reservado de `Info`. `finish` a secas deja el fichero
  igual de válido, sin duración declarada.

### La orden: `voltra record`

```bash
voltra record -o demo.mkv --size 640x360 --fps 30 --seconds 10
```

- **Vídeo:** la escena de demostración por el compositor CPU, convertida a I420,
  como `voltra render`.
- **Audio:** un tono mono por una pista del mezclador (plan 012) hacia una mezcla
  estéreo a 48 kHz, en bloques de 1024.
- **Marcas de sincronía:** cada segundo exacto, el fotograma que empieza en ese
  instante sale en blanco y el tono suena 50 ms empezando en la muestra exacta
  de ese segundo. Sirven para medir la sincronía A/V en el fichero, con una
  herramienta ajena.
- **Reloj:** el audio manda (CLAUDE.md §5). El bloque *k* lleva
  `rate.pts(k · 1024)` y el fotograma *n* `fps.pts(n)`; se escribe siempre el
  siguiente más temprano de los dos.

## Pasos

Tres commits:

1. **Documentos**: referencia, ADR 0005 y este plan.
2. **El muxer**, con tests de la codificación EBML, del orden, del corte de
   clusters y, si `ffprobe` está instalado, de que FFmpeg lee lo que escribe.
3. **`voltra record`**, con los tests de sincronía y de truncado; benchmark;
   `PERFORMANCE.md`, `ROADMAP.md`, `CLAUDE.md` §8 y este plan.

## Verificación

Con `ffprobe`/`ffmpeg` como oráculo; los tests que lo necesitan se saltan, y lo
dicen, cuando no están instalados.

- **FFmpeg lo lee:** dos pistas, `rawvideo` `yuv420p` al tamaño pedido y
  `pcm_f32le` estéreo a 48 kHz; tantos fotogramas como `segundos × fps`; y el
  fichero se decodifica entero sin errores.
- **Sincronía A/V, medida en el fichero:** se decodifica el vídeo a un píxel de
  luma por fotograma y el audio a muestras; cada destello y el comienzo de cada
  pitido coinciden **dentro de medio fotograma** (16,7 ms a 30 fps), y la
  diferencia no crece de un segundo al siguiente.
- **Resistente a cortes:** el fichero truncado a mitad se sigue leyendo y
  decodificando hasta el último bloque completo.
- EBML: enteros variables en el tamaño mínimo, el tamaño desconocido en 8 bytes,
  identificadores con su marca de longitud.
- Un bloque fuera de orden se rechaza; un salto de más de 1 s abre un cluster.
- Escribir audio no asigna tras el primer bloque.

**Rendimiento:** coste de muxear un fotograma I420 1080p y un bloque de audio,
anotado. No hay umbral que cumplir: el coste dominante de grabar sin comprimir es
el disco, como ya mostró Y4M (`PERFORMANCE.md` §3.5).

## Riesgos

- **Un reproductor no acepta clusters de tamaño desconocido.** Son legales en
  Matroska y FFmpeg los lee; si un reproductor importante no, el plan B es
  escribir cada cluster con tamaño conocido reservando 8 bytes y volviendo
  atrás al cerrarlo, cuando el destino lo permita.
- **`V_UNCOMPRESSED` es poco común.** Es un formato de prueba y de tubería, como
  Y4M; el formato de verdad llega con los encoders. FFmpeg lo lee, y eso basta
  para este paso.
- **CI sin FFmpeg.** Los tests de oráculo se saltan y lo dicen; el resto de la
  verificación no los necesita. Instalarlo en CI es una línea, si hace falta.
