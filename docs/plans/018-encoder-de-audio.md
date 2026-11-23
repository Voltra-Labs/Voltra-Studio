# 018 — Encoder de audio y entrelazado

- **Fase:** 4
- **Estado:** completado (2026-11-23) — ver Resultado. Ejecutado sin esperar
  revisión del plan, por indicación expresa del responsable ("no me preguntes,
  dale").

## Objetivo

`voltra record --video h264 --audio aac` graba un fichero comprimido en vídeo y
audio, con la sincronía A/V medida en él igual que hasta ahora. Para eso:
un trait `AudioEncoder` con AAC por `libavcodec` tras la feature `ffmpeg`; un
`Interleaver` en `voltra-output` que ordena los paquetes de varios encoders con
retardo; y Matroska con origen explícito, marcas negativas para el retardo
inicial y la pista AAC con su `CodecDelay`.

## Fuera de alcance

- **Opus.** Mismo trait y mismo vehículo; WHIP lo necesitará (fase 6). Exige
  intercalar las muestras —`libopus` no acepta float planar—, y se hará con su
  plan.
- **Audio multipista**, fotogramas B, y descartar hasta el primer clave.
- **Encoders en otros hilos.** El entrelazador es de quien escribe; cuando haya
  hilos, llegarán por colas.

## Referencia

`docs/references/audio-encoding.md`: los encoders de audio de OBS, su
entrelazado en `obs-output.c`, y cómo escribe FFmpeg el retardo inicial en
Matroska, comprobado aquí.

**Qué adoptamos:** tramas fijas selladas por un contador de muestras; la
convención de FFmpeg para el retardo —primer paquete antes del cero,
`CodecDelay` en la pista—; la regla de entrelazado de OBS; copiar el paquete al
encolarlo.

**Qué mejoramos:** un origen común en vez de poner a cero cada pista por su
cuenta; colas por pista en vez de un array ordenado; búferes reutilizados; sin
mutex.

## Diseño

### Matroska: origen explícito y marcas negativas

- `MatroskaWriter::set_origin(t)`, antes del primer bloque: el cero del fichero.
  Sin llamarlo, el origen sigue siendo el primer bloque.
- Un bloque anterior al origen lleva marca negativa: entra en el primer cluster
  (marca 0) con una marca relativa negativa, que el entero de 16 bits con signo
  admite. Así escribe FFmpeg el primer paquete de AAC.
- `TrackSpec::Aac { rate, layout, decoder_config, delay }` → `A_AAC`, el
  `AudioSpecificConfig` en `CodecPrivate` y `CodecDelay` en nanosegundos.

### `Interleaver`

```rust
pub struct Interleaver { ... }
impl Interleaver {
    pub fn new(tracks: usize) -> Self;
    pub fn push(&mut self, track: usize, pts: Timestamp, keyframe: bool, data: &[u8]);
    pub fn finish(&mut self, track: usize);
    pub fn drain(&mut self, write: impl FnMut(usize, Timestamp, bool, &[u8]) -> Result<()>) -> Result<()>;
}
```

- Una cola por pista; cada pista entrega en orden.
- `drain` escribe el paquete más temprano entre las cabezas mientras **todas las
  pistas no terminadas** tengan algo pendiente; una pista terminada no detiene a
  las demás. Al terminarlas todas, se vacía.
- `push` copia el paquete a un búfer sacado de un *pool*; al escribirse, vuelve
  al *pool*. En régimen estacionario no asigna.

### `AudioEncoder`

```rust
pub struct AudioSettings { pub rate: SampleRate, pub layout: ChannelLayout, pub bitrate_kbps: u32 }

pub trait AudioEncoder: Send {
    fn info(&self) -> &AudioEncoderInfo;   // codec, name, settings, frame size, delay
    fn headers(&self) -> Option<&[u8]>;    // AudioSpecificConfig
    fn encode(&mut self, block: &AudioBuffer, sink: &mut PacketSink<'_>) -> Result<()>;
    fn flush(&mut self, sink: &mut PacketSink<'_>) -> Result<()>;
}
```

- Acepta bloques de cualquier tamaño y acumula hasta la trama del encoder.
- La primera muestra del primer bloque fija el contador; cada trama se sella con
  su índice absoluto de muestra, y `libavcodec` resta el retardo. Por eso la
  marca de tiempo de la entrada tiene que ser al menos el retardo: con un reloj
  monótono siempre lo es, y `voltra record` arranca su línea de tiempo en 1 s.
- `FfmpegAudioEncoder` tras `ffmpeg`: el `aac` nativo de FFmpeg (LGPL, siempre
  presente), float planar —su formato, y el nuestro—, `GLOBAL_HEADER`. La última
  trama, corta, se envía tal cual: el encoder lo admite.

### `voltra record --audio aac --audio-bitrate 128`

- La línea de tiempo arranca en 1 s y el muxer recibe ese origen.
- Todos los paquetes —vídeo y audio, crudos o codificados— pasan por el
  `Interleaver`. Más sencillo que tener un bucle por combinación, y la copia de
  un fotograma crudo es aceptable en el formato de desarrollo que es.

## Pasos

Cuatro commits:

1. **Documentos**: referencia y este plan.
2. **`voltra-output`**: origen explícito, marcas negativas, pista AAC,
   `Interleaver`; con sus tests.
3. **`AudioEncoder` y AAC** por `libavcodec`, con sus tests.
4. **`voltra record --audio aac`** por el entrelazador: tests de FFmpeg,
   benchmark, documentación.

## Verificación

- **Entrelazado:** dos pistas que entregan con retardos distintos salen en orden
  global; una pista terminada no bloquea a la otra; nada se queda dentro al
  terminar; el *pool* no crece en régimen estacionario.
- **Marcas negativas:** un bloque 21 ms antes del origen se escribe en el primer
  cluster con marca −21, y FFmpeg lo lee como `start_time=-0.021`.
- **AAC:** tramas de 1024 muestras; retardo 1024; los paquetes cubren toda la
  entrada tras el vaciado; las cabeceras son un `AudioSpecificConfig` de 2 bytes
  (AAC-LC, 48 kHz, estéreo).
- **FFmpeg lee la grabación:** `h264` y `aac`, `initial_padding=1024`.
- **Sincronía A/V en el fichero:** el test de destellos y pitidos, con el mismo
  margen de medio fotograma. Si el retardo no se trata bien, falla por 21 ms.

**Rendimiento:** coste de codificar un bloque de 1024 muestras estéreo en AAC, y
del entrelazador por paquete, anotados.

## Riesgos

- **El pitido codificado empieza antes.** AAC esparce energía antes de un
  transitorio (pre-eco) dentro de su trama; el detector de inicio puede
  dispararse unos milisegundos antes. El margen es medio fotograma; se anota la
  cifra medida.
- **Marcas negativas en reproductores.** Es lo que escribe FFmpeg y lo que lee;
  si un reproductor importante no las admite, el plan B es desplazar todo el
  fichero por el retardo, como hace `avoid_negative_ts`.

## Resultado

Puerta de calidad en verde: **336 tests** por defecto (desde 325) y **47** con
`--features ffmpeg` en `voltra-encode` y `voltra-cli`. Cuatro commits:
documentos; entrelazador, origen y pista AAC; `AudioEncoder` con AAC; y
`voltra record --audio aac` por el entrelazador.

### Sincronía, medida con marcas reales

| Grabación | Audio − vídeo, segundos 1 a 5 |
|---|---|
| Vídeo crudo, PCM | 20,8 µs, constante |
| H.264, PCM | 20,8 µs, constante |
| **H.264, AAC** | **0,354 ms (17 muestras), constante** |

Los 20,8 µs son la muestra de fase cero del tono, como desde el plan 016. Los
0,35 ms de AAC son el códec: desplaza un poco el comienzo del pitido dentro de
su trama, y el detector lo ve 17 muestras más tarde. El margen era medio
fotograma, 16,7 ms. En ningún caso hay deriva.

### Coste

AAC: 958 µs por bloque de 1024 muestras estéreo, el 4,5 % de su duración. El
entrelazador: 119 ns por par de paquetes. Detalle y el experimento fallido con
`aac_coder=fast` en `PERFORMANCE.md`.

## Lo que encontraron las pruebas

Tres fallos, uno de ellos anterior a este plan:

1. **El convenio de `CodecDelay` estaba al revés en la referencia.** La leí en
   la salida de `ffprobe`, que da la marca ya corregida, y no en el fichero. Con
   el retardo restado en el encoder y otra vez en el demuxer, el AAC empezaba a
   −42 ms. Matroska define `CodecDelay` como lo que se **resta** de la marca
   almacenada: el muxer suma ahora el retardo a las marcas de la pista AAC, y el
   fichero coincide con el de FFmpeg. La referencia está corregida y lo dice.
2. **Los fotogramas de `voltra record` salían sellados sin el origen.** El sello
   se ponía antes de `rgb_to_yuv`, que copia el del lienzo. Con la línea de
   tiempo empezando en cero, como hasta el plan 017, coincidían; con el origen
   de 1 s, el vídeo entero quedaba un segundo antes del audio.
3. **El test de sincronía de los planes 016 y 017 no miraba las marcas de
   tiempo**: calculaba el instante de cada destello como índice ÷ 30 y el de cada
   pitido como muestra ÷ 48 000. Un desplazamiento de una pista entera —como el
   del punto 2— le pasaba inadvertido. Ahora usa la marca de presentación de
   cada fotograma decodificado y la del primer bloque de audio, que es lo que
   ve un reproductor; y decodifica sin `fps_mode` por defecto, porque FFmpeg
   duplicaba el primer fotograma para rellenar el hueco del cebado del audio y
   corría un índice todos los destellos. Con la medida nueva, las grabaciones
   de los planes 016 y 017 dan las mismas cifras que entonces: su sincronía era
   real, pero no estaba demostrada.

## Desviaciones respecto al plan

- **Las marcas negativas no hacen falta para AAC**, contra lo que decía el
  plan: con `CodecDelay` sumado, el primer bloque se almacena en el origen. El
  soporte queda —es parte de Matroska y tiene su test con FFmpeg— para lo que
  caiga antes del origen.
- **El `AudioSpecificConfig` de FFmpeg ocupa 5 bytes**, no 2: añade la extensión
  que señala que no hay SBR. Es lo que su muxer almacena, y lo que se guarda.
- **`voltra record` informa el pico de paquetes en cola** del entrelazador: el
  retardo de x264 medido en paquetes de audio.
- **`record.rs` se dividió** en `args`, `scene` y `pipeline`, por tamaño.
