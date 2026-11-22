# Estado del arte: grabar a fichero

Referencia para la fase 4 y su primer paso, el plan 016. Cómo escribe OBS un
fichero de grabación —en qué proceso, en qué contenedor y por qué— y qué parte
de eso se puede construir y comprobar antes de tener un solo encoder.

Fuentes (rama `master`, noviembre de 2026):

- [`frontend/widgets/OBSBasic.cpp`](https://github.com/obsproject/obs-studio/blob/master/frontend/widgets/OBSBasic.cpp) y [`OBSBasic.hpp`](https://github.com/obsproject/obs-studio/blob/master/frontend/widgets/OBSBasic.hpp) — el contenedor por defecto.
- [`plugins/obs-ffmpeg/obs-ffmpeg-mux.c`](https://github.com/obsproject/obs-studio/blob/master/plugins/obs-ffmpeg/obs-ffmpeg-mux.c) — el muxer de FFmpeg en un proceso aparte.
- [`plugins/obs-outputs/mp4-mux.c`](https://github.com/obsproject/obs-studio/blob/master/plugins/obs-outputs/mp4-mux.c) y [`mp4-output.c`](https://github.com/obsproject/obs-studio/blob/master/plugins/obs-outputs/mp4-output.c) — el MP4 "híbrido", escrito sin FFmpeg.
- [Matroska, especificación](https://www.matroska.org/technical/elements.html) (IETF RFC 9559) y [EBML](https://www.rfc-editor.org/rfc/rfc8794) (RFC 8794).

---

## 1. El problema que todos resuelven: un cuelgue no puede perder la grabación

Un MP4 clásico guarda su índice —el `moov`, que dice dónde está cada muestra—
**al final**, cuando ya se conocen todas. Si el programa se cuelga a la hora y
media, el fichero tiene hora y media de datos y ningún índice: no se reproduce.
La interfaz de OBS lo advierte así (`frontend/data/locale/en-US.ini`):

> Warning: Recordings saved to MP4/MOV will be unrecoverable if the file cannot
> be finalized (e.g. as a result of BSODs, power losses, etc.). If you want to
> record multiple audio tracks consider using MKV and remux the recording to
> MP4/MOV after it is finished.

Las dos respuestas que hay hoy en OBS atacan lo mismo desde lados distintos.

## 2. Aislar el muxer: `obs-ffmpeg-mux`

La salida de grabación basada en FFmpeg no llama a `libavformat` desde OBS.
Lanza un **ejecutable aparte**, `obs-ffmpeg-mux`, con los parámetros del
fichero en la línea de órdenes:

```c
#define FFMPEG_MUX "obs-ffmpeg-mux"
...
stream->pipe = os_process_pipe_create2(args, "w");
```

y le manda cada paquete codificado por un pipe: primero una cabecera con la
información del paquete y luego sus bytes (`write_packet`, dos llamadas a
`os_process_pipe_write`). El proceso hijo es el que tiene `libavformat` y
escribe el fichero.

Lo que compra: un fallo dentro de FFmpeg —un contenedor mal configurado, un
error de E/S, un `abort()`— termina el proceso hijo, no OBS. La emisión sigue.
Es el aislamiento de fallos que CLAUDE.md pide, aplicado a la dependencia más
grande del programa.

## 3. No depender del índice final: el MP4 híbrido

Desde 2024 OBS tiene un muxer MP4 propio, escrito en C sin FFmpeg (`mp4-mux.c`,
unas 3000 líneas). Su contenedor por defecto es ese:

```cpp
#define DEFAULT_CONTAINER "hybrid_mp4"   // "hybrid_mov" en macOS
```

"Híbrido" porque escribe **MP4 fragmentado** mientras graba —cada fragmento,
`moof` + `mdat`, es reproducible por sí mismo— y, al terminar bien, reescribe la
cabecera con un `moov` completo, de modo que el resultado es un MP4 normal.
Los fragmentos se cortan cuando todas las pistas alcanzan el fotograma clave en
el que toca cortar (`mp4_mux_submit_packet`, `mp4_flush_fragment`). Si OBS se
cuelga, queda un MP4 fragmentado: válido hasta el último fragmento escrito.

La lección no es "usa MP4": es que **el formato tiene que ser reproducible en
cada instante de la escritura**, y que para tenerlo OBS acabó escribiendo su
propio muxer en lugar de configurar el de FFmpeg.

## 4. Matroska: reproducible en cada instante por diseño

Matroska es EBML: elementos con un identificador, un tamaño y un contenido,
anidados. Dos propiedades lo hacen el formato natural para grabar:

- **Tamaño desconocido.** Un `Segment` y un `Cluster` pueden declarar su tamaño
  como "desconocido" (todos los bits del entero variable a 1) y terminar donde
  empieza el siguiente elemento de su nivel. Así se escribe en flujo, sin volver
  atrás, y un fichero cortado en cualquier punto es válido hasta el último
  bloque completo.
- **El índice es opcional.** Los `Cues` aceleran la búsqueda, pero un
  reproductor puede leer el fichero sin ellos.

Cada `SimpleBlock` lleva el número de pista, una marca de tiempo relativa al
`Cluster` (entero de 16 bits con signo) y los datos de un paquete. Las marcas de
tiempo son enteros en unidades de `TimestampScale` nanosegundos; el valor por
defecto, y el que usa FFmpeg, es 1 ms.

Comprobado en este contenedor con FFmpeg 6.1: Matroska con vídeo
**`V_UNCOMPRESSED`** (FourCC `I420` en `ColourSpace`) y audio
**`A_PCM/FLOAT/IEEE`** se lee, se identifica (`rawvideo`, `yuv420p`;
`pcm_f32le`) y se decodifica entero. Es decir: se puede grabar audio y vídeo
reales, reproducibles, **antes de tener ningún encoder**, y comprobar el fichero
con una herramienta independiente.

---

## 5. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:**

- El objetivo del MP4 híbrido: un fichero que es válido en todo momento mientras
  se escribe.
- Escribir el muxer propio en vez de envolver `libavformat`, como hizo OBS con
  el MP4. Un muxer es serialización de estructuras conocidas, no un codec.
- Aislar lo que sí necesita FFmpeg (los encoders) de forma que su fallo no
  arrastre al programa, como `obs-ffmpeg-mux`. Cómo —proceso aparte o crate tras
  una feature— se decide con el plan de encoders.

**Qué hacemos distinto:**

1. **Matroska primero, nativo y en flujo.** Segmento y clusters de tamaño
   desconocido: reproducible en cada byte escrito por la forma del formato, sin
   la doble escritura (fragmentado y luego reescrito) del MP4 híbrido. El MP4
   llega después, para quien lo necesite, como remuxeo.
2. **Sin dependencias nativas para escribir el fichero.** El muxer está en Rust,
   en `voltra-output`, y compila y se prueba en las features por defecto.
3. **Marcas de tiempo exactas desde el índice absoluto**, como todo reloj de
   Voltra: el bloque de audio *k* y el fotograma *n* se sellan desde su índice,
   nunca sumando duraciones redondeadas a milisegundos.
