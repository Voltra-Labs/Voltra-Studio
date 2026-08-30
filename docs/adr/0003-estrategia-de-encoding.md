# ADR 0003 — Encoding: hardware primero y sin copia a CPU

- **Estado:** aceptada (2026-08-30)
- **Fase:** 4

## Contexto

Se pidió "lo que más optimizado esté". Conviene precisar qué significa eso,
porque la intuición ("elegir el mejor encoder") es la respuesta equivocada.

En OBS, lo que decide el rendimiento del encoding no es el codec: es que los
encoders con `OBS_ENCODER_CAP_PASS_TEXTURE` reciben **la textura de GPU
directamente**, sin viaje de vuelta a memoria de sistema
(`docs/references/obs-studio.md` §4). Un NVENC alimentado por CPU rinde peor que
uno alimentado por textura, con el mismo codec y los mismos ajustes. La copia
GPU→CPU de un frame 4K son megabytes por frame y una sincronización que frena la
tubería entera.

## Decisión

La estrategia óptima es una **jerarquía**, no un encoder:

1. **Camino rápido — hardware con paso de textura (objetivo).** El frame no baja
   nunca de la GPU: compositor → conversión de color en GPU → encoder. En Linux
   eso es VAAPI (ADR 0002), con NVENC nativo como siguiente paso.
2. **Camino medio — hardware alimentado por memoria de sistema.** Cuando el
   compartido de texturas no está disponible. Descarga asíncrona con doble búfer
   para no serializar el pipeline.
3. **Respaldo — software (x264 / SVT-AV1).** Correcto y siempre disponible; se
   asume su coste de CPU.

**Vehículo:** `libavcodec` (FFmpeg) tras la feature `ffmpeg`, porque da VAAPI,
NVENC, QSV, x264 y el muxing MP4/MKV con una sola integración, y es lo que OBS
usa para buena parte de sus encoders. `voltra-encode` expone un trait
`VideoEncoder` propio; FFmpeg queda **detrás** de él, nunca en la API pública.
Así, una implementación nativa (NVENC directo, VideoToolbox) puede sustituirlo
sin tocar a los usuarios del trait.

El trait se diseña con la entrada como *frame que puede ser una textura*, no
como `&[u8]`. Si la API obliga a bytes en CPU, el camino rápido es imposible
después.

## Consecuencias

- FFmpeg es una dependencia nativa pesada: va tras feature flag y **no** puede
  entrar en las features por defecto (CI headless debe seguir compilando).
- Se necesita un banco de pruebas que mida las tres rutas con el mismo
  contenido: ms por frame, uso de CPU y bitrate real.
- Riesgo: acoplarse a las peculiaridades de `libavcodec` al definir el trait.
  Mitigación: el trait se valida en papel contra NVENC nativo y VideoToolbox
  antes de implementarse.
