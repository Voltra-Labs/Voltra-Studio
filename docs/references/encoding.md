# Estado del arte: el encoder de vídeo

Referencia para el plan 017. Cómo define OBS un encoder —qué entra, qué sale,
cuándo—, cómo usa x264 y cómo prepara H.264 para un contenedor. Y la
comprobación en papel que pide el ADR 0003 antes de fijar un trait: que la misma
forma sirva para x264, NVENC, VAAPI y VideoToolbox.

Fuentes (rama `master` de OBS, noviembre de 2026):

- [`libobs/obs-encoder.h`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-encoder.h) — `struct obs_encoder_info`, `struct encoder_packet`, las capacidades.
- [`plugins/obs-x264/obs-x264.c`](https://github.com/obsproject/obs-studio/blob/master/plugins/obs-x264/obs-x264.c) — el encoder por software.
- [`libobs/obs-avc.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-avc.c) — Annex B, `avcC`, fotogramas clave.
- ISO/IEC 14496-15 §5.3.3.1 (`AVCDecoderConfigurationRecord`) y la especificación de Matroska para `V_MPEG4/ISO/AVC`.

---

## 1. La interfaz de un encoder en libobs

`struct obs_encoder_info` es una tabla de punteros a función. Lo que importa:

```c
bool (*encode)(void *data, struct encoder_frame *frame,
               struct encoder_packet *packet, bool *received_packet);
bool (*encode_texture2)(void *data, struct encoder_texture *texture, int64_t pts,
                        uint64_t lock_key, uint64_t *next_key,
                        struct encoder_packet *packet, bool *received_packet);
bool (*get_extra_data)(void *data, uint8_t **extra_data, size_t *size);
bool (*get_sei_data)(void *data, uint8_t **sei_data, size_t *size);
uint32_t caps;   // OBS_ENCODER_CAP_PASS_TEXTURE, _DYN_BITRATE, _ROI, ...
```

Tres hechos de diseño:

1. **Un fotograma entra; puede no salir nada.** `received_packet` dice si esta
   llamada produjo un paquete. Un encoder con *lookahead* o fotogramas B retiene
   fotogramas y entrega con retardo: la salida no está alineada con la entrada.
2. **Las cabeceras van aparte.** `get_extra_data` devuelve los parámetros del
   codec (SPS y PPS en H.264) que el contenedor escribe una vez, al principio.
3. **La textura es otra entrada, no otro encoder.** `encode_texture2` recibe un
   *handle* de GPU; un encoder lo declara con `OBS_ENCODER_CAP_PASS_TEXTURE`. Es
   el camino rápido del ADR 0003.

El paquete de salida, `struct encoder_packet`: `data`/`size`, `pts`/`dts` con su
`timebase_num`/`timebase_den`, `keyframe`, y una `priority` para descartar con
criterio cuando la red no da abasto.

## 2. x264 en OBS

`obs-x264.c` usa `libx264` directamente, sin `libavcodec`. Lo relevante:

- Pide **Annex B** (`b_annexb = 1`): cada NAL precedida de un código de inicio
  `00 00 01` o `00 00 00 01`.
- Saca las cabeceras con `x264_encoder_headers` y separa las SEI de SPS/PPS
  (`load_headers`).
- El intervalo de fotogramas clave sale de `keyint_sec` × fps
  (`i_keyint_max = keyint_sec * fps_num / fps_den`).
- Entrega el paquete con las marcas de `pic_out` (`i_pts`, `i_dts`), que con
  fotogramas B no coinciden.

## 3. De Annex B a lo que pide un contenedor

Matroska y MP4 no quieren códigos de inicio: quieren cada NAL precedida de su
**longitud** (4 bytes big-endian) y las cabeceras en un
`AVCDecoderConfigurationRecord` (`avcC`) en la descripción de la pista.
`obs-avc.c` hace las dos conversiones:

- **`obs_parse_avc_header`** construye el `avcC`: versión 1, perfil,
  compatibilidad y nivel copiados del SPS, `0xFF` (longitudes de 4 bytes),
  `0xE1` (un SPS), el SPS con su longitud, un PPS con la suya y, para los
  perfiles High, los tres bytes de croma y profundidad que se leen del SPS
  quitando los bytes de prevención de emulación.
- **`obs_parse_avc_packet`** reescribe cada paquete con longitudes en vez de
  códigos de inicio, y de paso detecta si es clave (NAL de tipo 5, IDR). Lo
  hace **en un búfer nuevo por paquete** (`array_output_serializer_init`): una
  reserva de memoria por fotograma.

## 4. La misma forma para cuatro encoders: comprobación en papel

El ADR 0003 pide validar el trait contra NVENC y VideoToolbox antes de
implementarlo. Lo que cada API hace con las cuatro preguntas del trait:

| | x264 (por `libavcodec`) | NVENC | VAAPI (por `libavcodec`) | VideoToolbox |
|---|---|---|---|---|
| **Entrada** | fotograma en memoria | superficie de memoria o textura registrada | superficie VA (o memoria, subida) | `CVPixelBuffer`, que puede ser una `IOSurface` de GPU |
| **¿Sale un paquete por fotograma?** | no: *lookahead* y fotogramas B retrasan | no: retraso configurable | no | no: es asíncrono, por callback |
| **Cabeceras** | `extradata` al abrir, con `GLOBAL_HEADER` | `nvEncGetSequenceParams` tras inicializar | `extradata` al abrir | en el `CMFormatDescription` del **primer** paquete |
| **Formato del flujo** | Annex B | Annex B | Annex B | longitudes (AVCC) |
| **Vaciado al final** | enviar "fin" y recoger | `EOS` | enviar "fin" y recoger | `CompleteFrames` |

Consecuencias para el trait:

1. **La salida es un flujo, no un retorno.** `encode` entrega cero o más paquetes
   a quien llama; `flush` entrega los retenidos. Ninguna API da exactamente uno
   por fotograma.
2. **Las cabeceras pueden no existir hasta el primer paquete** (VideoToolbox).
   `headers()` devuelve `Option`, y quien escribe el contenedor espera.
3. **El formato del flujo lo declara el encoder** (Annex B o longitudes), y la
   conversión es una utilidad aparte. Imponer uno obligaría a VideoToolbox a
   convertir para que el muxer reconvierta.
4. **La entrada es un enum**: un fotograma en memoria hoy, una textura cuando
   exista el camino rápido. Es lo que el ADR 0003 pide: si el tipo de entrada
   es `&[u8]`, la textura no cabe después.
5. **Las marcas de tiempo salen del encoder**, no se copian de la entrada: con
   fotogramas B, el paquete *k* no es el fotograma *k*.

---

## 5. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:**

- La forma de `obs_encoder_info`: entrada que puede no producir salida,
  cabeceras aparte, la textura como otra entrada del mismo encoder.
- `obs-avc.c` como especificación de las conversiones: el `avcC`, con la
  extensión de los perfiles High, y Annex B a longitudes.
- Los valores por defecto de `obs-x264` donde no hay razón para otros
  (`obs_x264_defaults`): preset `veryfast`, CBR a 6000 kbps, perfil vacío —el
  que elige x264, High—.

**Qué hacemos distinto:**

1. **Los paquetes se prestan, no se copian.** El trait entrega cada paquete como
   `&[u8]` válido durante la llamada, directamente desde el `AVPacket`, y el
   muxer lo escribe sin copiarlo.
2. **Annex B a longitudes sin asignar por fotograma**: en un búfer reutilizado,
   frente a la reserva por paquete de `obs_parse_avc_packet`.
3. **Las cabeceras pueden llegar tarde por contrato** (`Option`), en vez de
   suponer que existen al abrir.
4. **`libavcodec` tras un trait y una feature** (ADR 0003): x264 hoy; VAAPI y
   NVENC son otro nombre de encoder en la misma integración, sin tocar el
   trait.
