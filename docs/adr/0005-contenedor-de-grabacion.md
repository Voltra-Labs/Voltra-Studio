# ADR 0005 — Grabación: Matroska nativo, en flujo

- **Estado:** aceptada (2026-11-22)
- **Fase:** 4

## Contexto

La fase 4 graba a fichero. El ADR 0003 eligió `libavcodec` tras una feature
como vehículo para los **encoders**, y de paso mencionó el muxing MP4/MKV de
FFmpeg. Ese "de paso" merece una decisión propia, porque el contenedor
condiciona dos cosas que no dependen del codec:

1. **Qué queda si el programa se cae.** Un MP4 clásico sin finalizar no se
   reproduce; OBS lo advierte en su interfaz y acabó escribiendo su propio
   muxer MP4 "híbrido" para evitarlo (`docs/references/recording.md` §1, §3).
2. **Qué se puede probar sin dependencias nativas.** Si el muxer es
   `libavformat`, no hay grabación en las features por defecto ni en CI.

## Opciones evaluadas

| Opción | A favor | En contra |
|---|---|---|
| **`libavformat` (FFmpeg)** para todo | Todos los contenedores; lo mismo que OBS en `obs-ffmpeg-mux` | Dependencia nativa en el camino de cualquier grabación; sin ella no hay ni un fichero; un fallo dentro tumba el proceso salvo que se aísle en otro |
| **MP4 propio** (como el híbrido de OBS) | Lo que espera todo el mundo | Para sobrevivir a un cuelgue hay que escribirlo fragmentado y reescribirlo al final; ~3000 líneas en OBS |
| **Matroska propio, en flujo** | Reproducible en cada byte escrito por la forma del formato (tamaños desconocidos); EBML es pequeño; transporta vídeo sin comprimir y PCM, así que se graba antes de tener encoders | Algunos destinos piden MP4: hará falta un remuxeo o un segundo muxer |

## Decisión

**Matroska, escrito en Rust en `voltra-output`, en flujo**: `Segment` y
`Cluster` de tamaño desconocido, sin volver atrás para escribir. El fichero es
válido hasta el último bloque completo en cualquier momento.

- El muxer no depende de FFmpeg ni de nada nativo: compila y se prueba en las
  features por defecto.
- Los encoders siguen el ADR 0003 y entregan paquetes a este muxer; FFmpeg, si
  entra, entra por los encoders.
- **MP4** llega después como remuxeo o como segundo muxer, cuando un destino lo
  pida. No es el formato de grabación.

## Consecuencias

- Un fichero cortado por un cuelgue se reproduce hasta donde llegó, sin ninguna
  recuperación. Se prueba truncando ficheros.
- Sin `Cues`, buscar en un fichero largo es más lento: el reproductor tiene que
  recorrerlo. Escribir el índice al cerrar, cuando el destino admite volver
  atrás, es una mejora posterior que no cambia esta decisión.
- `ffprobe`/`ffmpeg`, cuando están instalados, son el oráculo independiente de
  los tests: lo que FFmpeg lee es lo que leerán los reproductores.
