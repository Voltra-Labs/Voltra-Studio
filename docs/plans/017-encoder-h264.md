# 017 — Encoder H.264

- **Fase:** 4
- **Estado:** en curso (2026-11-22) — ejecutado sin esperar revisión del plan,
  por indicación expresa del responsable ("no me preguntes, dale")

## Objetivo

La crate `voltra-encode` con el trait `VideoEncoder` del ADR 0003, las
utilidades de flujo H.264 en Rust puro, y un primer encoder real —x264 a través
de `libavcodec`, tras la feature `ffmpeg`—; Matroska aprende la pista H.264, y
`voltra record --video h264` graba comprimido, con la sincronía A/V medida
igual que en el plan 016.

## Fuera de alcance

- **Hardware** (VAAPI, NVENC): mismo vehículo, otro nombre de encoder. Este
  contenedor no tiene GPU para probarlo.
- **El camino de textura** (ADR 0003, camino rápido). El trait lo admite en su
  tipo de entrada; nadie lo implementa todavía.
- **Fotogramas B.** Con ellos el orden de decodificación no es el de
  presentación, y el muxer exige marcas no decrecientes. Se desactivan; volver a
  activarlos es un cambio del muxer (ordenar por DTS), con su plan.
- **Encoder de audio** (AAC, Opus): el plan siguiente, con el entrelazador que
  necesitarán dos encoders con retardo.
- **Aislar FFmpeg en otro proceso**, como `obs-ffmpeg-mux`. Se anota como opción
  para cuando haya un fallo de FFmpeg que aislar; hoy la feature basta para que
  CI y las features por defecto no dependan de él.

## Referencia

`docs/references/encoding.md`: la interfaz de libobs, `obs-x264`, `obs-avc.c` y
la comprobación en papel del trait contra x264, NVENC, VAAPI y VideoToolbox.

**Qué adoptamos:** la forma de `obs_encoder_info` —entrada sin salida
garantizada, cabeceras aparte, textura como otra entrada—; `obs-avc.c` como
especificación del `avcC` y de Annex B a longitudes; los valores por defecto de
`obs-x264` (preset `veryfast`, CBR).

**Qué mejoramos:** paquetes prestados en vez de copiados; la conversión a
longitudes en un búfer reutilizado, frente a la reserva por paquete de
`obs_parse_avc_packet`; cabeceras que pueden llegar tarde por contrato.

## Diseño

### `voltra-encode`, sin features

```rust
#[non_exhaustive]
pub enum EncoderInput<'a> { Frame(&'a VideoFrame) }   // Texture(..) later

pub struct EncodedPacket<'a> {
    pub data: &'a [u8],
    pub pts: Timestamp,
    pub dts: Timestamp,
    pub keyframe: bool,
}

#[non_exhaustive]
pub enum Bitstream { AnnexB, LengthPrefixed }

pub trait VideoEncoder: Send {
    fn info(&self) -> &EncoderInfo;        // codec, name, bitstream, size, fps
    fn headers(&self) -> Option<&[u8]>;    // None until known
    fn encode(&mut self, input: EncoderInput<'_>,
              sink: &mut dyn FnMut(EncodedPacket<'_>) -> Result<()>) -> Result<()>;
    fn flush(&mut self, sink: &mut dyn FnMut(EncodedPacket<'_>) -> Result<()>) -> Result<()>;
}
```

El `sink` es `dyn` a propósito: se llama una vez por paquete, no por píxel
(CLAUDE.md §4.4). El paquete vive lo que dura la llamada, y así va del
`AVPacket` al muxer sin copia.

Y `voltra_encode::h264`, puro y probado sin FFmpeg:

- `nal_units(annex_b)`: iterador sobre las NAL de un flujo Annex B, con códigos
  de inicio de 3 o 4 bytes.
- `to_length_prefixed(annex_b, &mut Vec<u8>)`: reescribe en un búfer del
  llamante, que tras el primer fotograma no crece.
- `decoder_config(annex_b_headers) -> Vec<u8>`: el `avcC`, con la extensión de
  los perfiles High leída del SPS sin bytes de prevención.

### El encoder x264, tras `ffmpeg`

`FfmpegVideoEncoder` sobre `ffmpeg-sys-next` 6.1 (bindings generados de las
cabeceras del sistema; solo `avcodec`). Todo el `unsafe` en un módulo, con
`// SAFETY:` por bloque.

- Abre `libx264` con `GLOBAL_HEADER` (cabeceras en `extradata`, no repetidas en
  cada clave), preset `veryfast`, CBR al bitrate pedido, GOP de 2 s y
  `max_b_frames = 0`.
- `time_base` = 1/fps exacto: la marca de entrada es el índice del fotograma, y
  la de salida vuelve a `Timestamp` con `fps.pts(índice)`, sin redondeo.
- La entrada se copia plano a plano a un `AVFrame` reutilizado. Es la única
  copia, y es inevitable mientras la entrada sea memoria: x264 retiene
  fotogramas para su *lookahead*. El camino sin copia es el de textura.
- La salida se recibe en un `AVPacket` reutilizado y se presta al `sink`.

### Matroska

`TrackSpec::H264 { size, fps, decoder_config }` → `CodecID` `V_MPEG4/ISO/AVC`,
`CodecPrivate` = `avcC`. `TrackSpec` deja de ser `Copy` (lleva el `avcC`).

### `voltra record --video h264 --bitrate 6000`

El primer paquete de vídeo llega con retardo, y el muxer exige orden. El bucle
pasa a ir guiado por el vídeo: por cada paquete con marca *P* se escriben antes
los bloques de audio con marca ≤ *P*, y luego el paquete; al final, el vaciado
del encoder y el audio que quede. Sin feature, `--video h264` se rechaza con una
explicación, como `--gpu`.

### CI

Un trabajo nuevo instala las cabeceras de FFmpeg y `libclang`, y corre clippy y
los tests de `voltra-encode` y `voltra-cli` con `--features ffmpeg`. El trabajo
principal sigue sin FFmpeg.

## Pasos

Cuatro commits:

1. **Documentos**: referencia y este plan.
2. **`voltra-encode`**: trait, tipos, `h264` con sus tests.
3. **x264 por `libavcodec`** tras `ffmpeg`, con tests y el trabajo de CI.
4. **Matroska H.264 y `voltra record --video h264`**: tests con FFmpeg,
   benchmark, documentación.

## Verificación

- `h264`: NAL con códigos de 3 y 4 bytes; un flujo de dos fotogramas sale con
  longitudes exactas; el `avcC` de un SPS High conocido coincide byte a byte con
  el que escribe FFmpeg para el mismo flujo; el búfer no crece tras el primer
  fotograma.
- **x264**: 60 fotogramas dan 60 paquetes tras el vaciado; el primero es clave
  y cada 2 s otro; las marcas de salida son las de entrada.
- **FFmpeg lee la grabación**: `h264`, perfil High, el tamaño pedido, todos los
  fotogramas; decodifica sin avisos.
- **Sincronía A/V en el fichero comprimido**: el mismo test de destellos y
  pitidos del plan 016, con el mismo margen.
- **Tamaño**: 10 s a 640×360 sin comprimir frente a H.264, anotado.

**Rendimiento:** milisegundos por fotograma 1080p de x264 `veryfast` en esta
máquina, anotados contra el presupuesto de 16,6 ms. Es el número que dirá cuánto
importa el hardware.

## Riesgos

- **`ffmpeg-sys-next` y la versión de FFmpeg del sistema.** Los bindings se
  generan de las cabeceras instaladas; si CI tiene otra versión, se fija la
  imagen o la versión del crate. Comprobado aquí con FFmpeg 6.1.
- **x264 a 1080p no cabe en 16,6 ms en CPU.** Es probable en una CPU de cuatro
  hilos; es exactamente por qué el ADR 0003 pone el hardware primero. Se anota
  el número.
- **El `avcC` no coincide con el de FFmpeg.** El test lo compara con lo que
  escribe `ffmpeg` para el mismo flujo; si difiere, manda el de FFmpeg, que es el
  que leen los reproductores.
