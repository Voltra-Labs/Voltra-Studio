# Estado del arte: YUV4MPEG2 (Y4M)

Referencia para el plan 009, el primer paso que produce un fichero
reproducible. Y4M es el formato de vídeo crudo que usan las tuberías de
mjpegtools, `ffmpeg`, `x264`, `x265`, `vpxenc`, `aomenc` y `mpv`: una cabecera
de texto, y detrás los planos en bruto, uno detrás de otro.

Se elige como **primera salida** por tres razones:

1. **No necesita dependencias.** El codificador entero cabe en unas decenas de
   líneas: `format!` para la cabecera y `write_all` para los planos. Meter
   FFmpeg o GStreamer ahora sería arrastrar un árbol de dependencias nativas
   antes de tener nada que enseñar.
2. **Es la entrada natural de todo lo demás.** `voltra render | x264 --demuxer
   y4m - -o salida.mp4` funciona sin que exista todavía `voltra-encode`. El
   camino a un MP4 queda abierto sin comprometer el diseño del encoder.
3. **Es verificable byte a byte.** Sin contenedor, sin entropía, sin
   codificación con pérdidas: lo que se escribe es exactamente lo que compuso el
   compositor, así que un test puede volver a leerlo y comparar píxel a píxel
   con el lienzo de origen.

Su coste es el que cabe esperar: 1080p60 en 4:2:0 son **187 MB/s**. Es un
formato para desarrollo, pruebas y tuberías, no para grabar una sesión.

---

## 1. La especificación (`yuv4mpeg(5)`, mjpegtools)

Fuente: [`yuv4mpeg(5)`](https://man.archlinux.org/man/extra/mjpegtools/yuv4mpeg.5.en).

Un STREAM es una STREAM-HEADER seguida de un número ilimitado de FRAMEs.

```
STREAM-HEADER := "YUV4MPEG2" (" " TAGGED-FIELD)* "\n"
FRAME         := "FRAME" (" " TAGGED-FIELD)* "\n" <datos del frame>
```

Un TAGGED-FIELD es una letra seguida de su valor, sin espacio entre medias.

| Tag | Significado | Obligatoriedad | Valores |
|---|---|---|---|
| `W` | Anchura en píxeles, > 0 | **Requerido** | entero |
| `H` | Altura en píxeles, > 0 | **Requerido** | entero |
| `F` | Cadencia, `num:den` | Por defecto `0:0` (desconocida) | ratio |
| `I` | Entrelazado | Por defecto `?` | `?` desconocido, `p` progresivo, `t` campo superior primero, `b` campo inferior primero, `m` mixto |
| `A` | Relación de aspecto **del píxel**, `num:den` | Por defecto `0:0` (desconocida) | ratio |
| `C` | Submuestreo de croma | Por defecto `420jpeg` | `420jpeg`, `420mpeg2`, `420paldv`, `411`, `422`, `444`, `444alpha`, `mono` |
| `X` | Metadatos | Opcional | cadena sin interpretar; los filtros la propagan |

Los datos del frame son planares, fila a fila, muestras de 8 bits en espacio
CCIR-601 Y'CbCr, en el orden **Y', Cb, Cr** (y alfa detrás en `444alpha`). En
4:2:0 cada plano de croma ocupa `(anchura × altura) / 4` octetos.

**El detalle que importa: el sitio del croma.** Los tres códigos 4:2:0 se
diferencian solo en dónde se considera situada la muestra de croma respecto a
las cuatro de luma:

| Código | Sitio | Quién lo usa |
|---|---|---|
| `420jpeg` | Centro del bloque 2×2 | JPEG, MPEG-1 |
| `420mpeg2` | Alineado horizontalmente con la columna izquierda | MPEG-2, H.264, HEVC |
| `420paldv` | Alineado además verticalmente por campo | PAL-DV |

No es un adorno: el escalador de croma del reproductor lo usa, y equivocarse
desplaza el color medio píxel respecto a la luma.

## 2. Lo que escribe FFmpeg

Fuente: [`libavformat/yuv4mpegenc.c`](https://ffmpeg.org/doxygen/trunk/yuv4mpegenc_8c_source.html).

La cabecera sale de una única cadena de formato:

```c
"YUV4MPEG2 W%d H%d F%d:%d I%c A%d:%d%s%s\n"
```

Es decir, `W H F I A` en ese orden fijo, y detrás dos cadenas ya montadas: la
del espacio de color (por ejemplo `" C420jpeg XYSCSS=420JPEG"`) y la del rango
(`" XCOLORRANGE=LIMITED"`, `" XCOLORRANGE=FULL"`, o nada).

Dos convenciones que no están en la especificación pero que el ecosistema
entiende, ambas montadas sobre el tag `X` de metadatos libres:

- **`XYSCSS=`** — el código de croma repetido en mayúsculas, para herramientas
  antiguas de mjpegtools anteriores al tag `C`.
- **`XCOLORRANGE=`** — `LIMITED` o `FULL`. La especificación **no tiene campo
  de rango**: dice "CCIR-601" y ya. Sin este tag, un reproductor supone rango
  limitado y una fuente de rango completo se ve lavada.

Y4M tampoco tiene campo de **primarios ni de matriz**: nada distingue BT.601 de
BT.709. La convención de facto de los reproductores es decidirlo por la altura
—BT.601 por debajo de 720 líneas, BT.709 a partir de ahí—, que es exactamente
lo que hace `mpv` al abrir un fichero sin señalizar.

Por frame, FFmpeg escribe `"FRAME\n"` y después los planos en bruto, uno a uno.

## 3. Salidas en libobs, y por qué aquí no hay todavía un trait

Fuente: `docs/references/obs-studio.md` §1 y §4.

En libobs una salida es un `obs_output_info` con `start`, `stop`, y callbacks
`raw_video`/`encoded_packet` según reciba frames o paquetes. Un mismo objeto
sirve para fichero, RTMP y salida en bruto, y el núcleo no distingue.

**Aquí no se define ese trait todavía**, y es una decisión, no un olvido: hay
**una** implementación. Un trait derivado de un único caso codifica las
casualidades de ese caso. El muxer a fichero (plan de la fase 4) y el
transporte RTMP (fase 6) tienen requisitos que Y4M no tiene —paquetes en vez de
frames, reconexión, bitrate adaptativo, cola con descarte— y el trait se
escribirá cuando existan dos de esos tres, con `obs_output_info` delante como
referencia.

## 4. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:**

- El orden de tags de FFmpeg (`W H F I A C`), porque es lo que todo el mundo
  ha probado contra todos los lectores que existen.
- `XYSCSS=` y `XCOLORRANGE=`, por compatibilidad y por honestidad sobre el
  rango.
- Un `write_all` por plano, no por fila, cuando el plano es contiguo.

**Qué hacemos distinto:**

1. **El código de croma se deriva del conversor, no se elige a mano.** El
   plan 004 promedia las cuatro muestras RGB del bloque 2×2 *antes* de
   convertir, lo que sitúa el croma en el **centro del bloque**: eso es
   `420jpeg`, y es lo que se escribe. Poner `420mpeg2` porque "es lo normal en
   H.264" sería mentir medio píxel.
2. **El rango se escribe siempre.** FFmpeg omite `XCOLORRANGE` cuando el rango
   es desconocido; nosotros conocemos el nuestro —viene del `ColorSpec` con el
   que se convirtió— así que no hay caso en el que falte.
3. **La escritura no asigna por frame.** La cabecera se monta una vez, en la
   construcción; el marcador `FRAME\n` es una constante de bytes. Por frame no
   hay ni un `format!` (CLAUDE.md §4.1).
4. **El stride no se filtra al fichero.** Nuestros frames se asignan con los
   planos alineados a 32 bytes y con relleno entre planos; el escritor emite
   `row_bytes` por fila, nunca `stride`. Es la clase de error que produce un
   fichero que casi se ve bien.
