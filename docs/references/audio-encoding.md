# Estado del arte: el encoder de audio y el entrelazado

Referencia para el plan 018. Dos problemas que aparecen a la vez en cuanto hay
un segundo encoder: cómo se codifica el audio —en tramas fijas, con un retardo
inicial— y cómo se ordenan los paquetes de dos encoders que entregan cada uno
con su retardo.

Fuentes (rama `master` de OBS y FFmpeg 6.1 de este contenedor, noviembre de
2026):

- [`plugins/obs-ffmpeg/obs-ffmpeg-audio-encoders.c`](https://github.com/obsproject/obs-studio/blob/master/plugins/obs-ffmpeg/obs-ffmpeg-audio-encoders.c) — AAC y Opus por `libavcodec`.
- [`libobs/obs-output.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-output.c) — `interleave_packets`, `initialize_interleaved_packets`, `apply_interleaved_packet_offset`.
- La especificación de Matroska: `CodecDelay`, `A_AAC`, y lo que escribe FFmpeg, comprobado aquí.

---

## 1. Un encoder de audio trabaja en tramas, y empieza tarde

AAC codifica en tramas de **1024 muestras** —el comentario de OBS dice "pretty
much always 1024 for AAC"—, y no en los bloques que le lleguen. OBS acumula las
muestras hasta tener una trama (`enc->samples`, `frame_size`) y sella cada una
con un contador de muestras propio:

```c
enc->aframe->pts = av_rescale_q(enc->total_samples, (AVRational){1, sample_rate}, ...);
enc->total_samples += enc->frame_size;
```

Su bitrate por defecto es **128 kbps** (`obs_data_set_default_int(settings,
"bitrate", 128)`).

Y todo encoder con ventana —AAC por su transformada solapada, Opus por su
predicción— necesita muestras **antes** de la primera que se le da: el
*priming* o retardo inicial. `libavcodec` lo publica en `initial_padding`, y su
primer paquete sale sellado **antes del cero**: lleva las muestras de cebado.
Comprobado con FFmpeg 6.1 en este contenedor, escribiendo Matroska con su propio
muxer y leyéndolo con `ffprobe`:

| | AAC (`aac`) | Opus (`libopus`) |
|---|---|---|
| `initial_padding` | 1024 muestras | 312 muestras |
| Marca del primer paquete, **según el demuxer** | −21 ms | −7 ms |
| Marca del primer bloque, **almacenada** | 0 | 0 |
| `CodecDelay` escrito | 21 333 333 ns | 6 500 000 ns |

La especificación de Matroska define `CodecDelay` como el retardo del codec que
**se resta de la marca de cada bloque** para obtener la que se reproduce. Por
eso FFmpeg almacena el primer bloque en 0 —la marca del paquete más el retardo—
y su demuxer lo devuelve a −21 ms; el decodificador descarta el cebado, y la
primera muestra que suena cae en el cero. Las dos mitades van juntas: almacenar
la marca del paquete sin sumarle el retardo, con `CodecDelay` declarado,
adelanta el audio 21 ms; omitir `CodecDelay` lo retrasa lo mismo.

*Corrección.* La primera versión de esta referencia leyó la fila del demuxer como
si fuera la marca almacenada, y el plan 018 la siguió. La grabación lo destapó:
`ffprobe` daba al AAC un inicio de −42 ms, el retardo restado dos veces. El
muxer suma ahora `CodecDelay` a las marcas de la pista, y el fichero coincide con
el de FFmpeg (inicio a −21 ms al leerlo).

## 2. Entrelazar en OBS

`interleave_packets` recibe los paquetes de todos los encoders de una salida.
Lo que hace, en orden:

1. **Hasta tener audio y vídeo, acumula.** El primer vídeo tiene que ser clave;
   lo anterior se descarta (`discard_unused_audio_packets`).
2. **Poda y alinea el arranque** (`prune_interleaved_packets`,
   `initialize_interleaved_packets`): descarta el audio anterior al primer
   vídeo y comprueba que haya audio más allá de él.
3. **Pone cada pista a cero en su primer paquete**, por separado:

   ```c
   /* audio and video need to start at timestamp 0, and the encoders
    * may not currently be at 0 when we get data.  so, we store the
    * current dts as offset and subtract that value from the dts/pts
    * of the output packet. */
   offset = (out->type == OBS_ENCODER_VIDEO) ? output->video_offsets[...]
                                             : output->audio_offsets[...];
   ```

   donde el desplazamiento del vídeo es la `pts` de su primer paquete y el del
   audio la `dts` del suyo.
4. **Inserta ordenado por `dts`** en un array (`insert_interleaved_packet`), y
   envía el primero cuando hay paquetes del otro tipo con una marca mayor
   (`count_streamable_frames`): el siguiente paquete de cada pista no puede ser
   anterior a lo que ya se envió.

Todo bajo `interleaved_mutex`, y con una copia de cada paquete
(`obs_encoder_packet_create_instance`), porque el encoder reutiliza su búfer.

---

## 3. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:**

- Acumular hasta una trama completa y sellar cada trama desde un contador de
  muestras, no desde los bloques de entrada.
- La convención de Matroska para el retardo, tal como la escribe FFmpeg:
  `CodecDelay` en la pista y cada bloque almacenado a la marca de su paquete más
  ese retardo. Es lo que leen los reproductores.
- La regla de entrelazado de OBS: se envía el paquete más temprano cuando todas
  las pistas tienen algo pendiente, porque cada pista entrega en orden.
- Copiar el paquete al encolarlo: el encoder presta su búfer y lo reutiliza.

**Qué hacemos distinto:**

1. **Un único origen para todas las pistas**, en vez de poner a cero cada una en
   su primer paquete. Con marcas absolutas en un mismo reloj, el muxer recibe
   un cero explícito —el instante en que empieza la grabación—, y ninguna
   diferencia entre los arranques de los encoders se convierte en un desfase
   A/V. Lo que caiga antes del origen se escribe con marca negativa.
2. **Cola por pista en vez de un array ordenado global.** Cada pista entrega en
   orden, así que basta con mirar la cabeza de cada una; insertar ordenado en un
   array, como OBS, desplaza memoria en cada paquete.
3. **Búferes de la cola reutilizados**: la copia de cada paquete va a un búfer
   que vuelve a un *pool* al escribirse, sin una reserva por paquete.
4. **Sin mutex**: el entrelazador pertenece a quien escribe el fichero. Si
   algún día los encoders viven en otros hilos, llegarán por colas SPSC.

**Qué queda fuera:** descartar hasta el primer fotograma clave (los encoders de
Voltra empiezan siempre en uno) y el audio multipista.
