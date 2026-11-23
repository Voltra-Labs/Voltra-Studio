# 018 — Encoder de audio y entrelazado

- **Fase:** 4
- **Estado:** en curso (2026-11-23) — ejecutado sin esperar revisión del plan,
  por indicación expresa del responsable ("no me preguntes, dale")

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
