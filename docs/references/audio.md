# Estado del arte: representación de audio

Referencia para el plan 011, el primer paso de la fase 3. El pipeline de audio de
OBS —el modelo *pull*, la alineación por marca de tiempo, el buffering de ~1 s—
ya está en `docs/references/obs-studio.md` §3. Este documento cubre lo anterior a
todo eso: **cómo se representan las muestras**.

---

## 1. Los tipos de libobs

Fuente: [`libobs/media-io/audio-io.h`](https://github.com/obsproject/obs-studio/blob/master/libobs/media-io/audio-io.h).

```c
enum audio_format {
    AUDIO_FORMAT_UNKNOWN,
    AUDIO_FORMAT_U8BIT,  AUDIO_FORMAT_16BIT,  AUDIO_FORMAT_32BIT,  AUDIO_FORMAT_FLOAT,
    AUDIO_FORMAT_U8BIT_PLANAR, AUDIO_FORMAT_16BIT_PLANAR,
    AUDIO_FORMAT_32BIT_PLANAR, AUDIO_FORMAT_FLOAT_PLANAR,
};

enum speaker_layout {
    SPEAKERS_UNKNOWN, SPEAKERS_MONO, SPEAKERS_STEREO, SPEAKERS_2POINT1,
    SPEAKERS_4POINT0, SPEAKERS_4POINT1, SPEAKERS_5POINT1, SPEAKERS_7POINT1,
};

#define MAX_AUDIO_MIXES     6
#define MAX_AUDIO_CHANNELS  8
#define AUDIO_OUTPUT_FRAMES 1024
```

Y el portador de muestras:

```c
struct audio_data {
    uint8_t *data[MAX_AV_PLANES];
    uint32_t frames;
    uint64_t timestamp;
};
```

Tres decisiones que importan:

1. **Internamente todo es `FLOAT_PLANAR`.** Las fuentes entregan en el formato
   que sea; `obs_source_output_audio` convierte y remuestrea, y a partir de ahí
   el núcleo solo ve float de 32 bits, un plano por canal. Los otros ocho
   valores del enum existen para el **borde**: lo que entrega un dispositivo y
   lo que espera un codificador.
2. **1024 muestras por bloque**, 21,3 ms a 48 kHz. Es la unidad de trabajo del
   mezclador.
3. **Ocho canales como máximo**, y seis mezclas independientes —para mandar una
   mezcla distinta a la grabación y a cada destino de streaming—.

## 2. Por qué float, y por qué planar

**Float**, porque mezclar es sumar y sumar entero desborda. Con `i16`, dos
fuentes al 80 % ya saturan y el recorte es irreversible; en `f32` la suma vive
tranquilamente fuera de `[-1, 1]` y solo se limita **una vez**, al convertir para
salida. Es lo que hacen todos: CoreAudio, PipeWire, JACK, WASAPI en modo
compartido, y toda estación de audio digital.

**Planar**, por dos razones concretas:

- Un filtro trabaja sobre un canal. Con planos, un canal es un `&[f32]`
  contiguo, que vectoriza solo; intercalado, el mismo bucle lee con paso 2, 6 u
  8 y desperdicia cada línea de caché que toca.
- El volumen y el paneo son por canal. Multiplicar un plano por un escalar es
  el bucle más simple que existe.

El coste es real y hay que decirlo: **el borde siempre está intercalado**. Los
dispositivos entregan intercalado, los codificadores casi siempre lo quieren
intercalado, y WAV y la mayoría de contenedores lo almacenan intercalado. Cada
cruce cuesta una transposición.

## 3. El tiempo del audio es exacto, y por eso manda

Un frame de vídeo a 30000/1001 no cae en un número entero de nanosegundos: por
eso el reloj del plan 002 es racional. El audio no tiene ese problema — a 48 kHz,
la muestra *n* está en `n / 48000` segundos exactos — pero **sí tiene el
problema del redondeo si se acumula**: sumar 21,333 ms mil veces no da lo mismo
que calcular `1_024_000 / 48_000`.

De ahí la regla que CLAUDE.md §5 ya fija: **el audio manda en la sincronía A/V y
el vídeo se ajusta a él.** El reloj de audio es el único del sistema que avanza
en unidades que el hardware cuenta de verdad, una muestra cada vez. El vídeo
tiene *frames*, que son una convención; el audio tiene *muestras*, que son
electricidad.

## 4. Cómo mezcla libobs

Fuente: [`libobs/obs-audio.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-audio.c),
función `mix_audio()`.

El núcleo de la mezcla es literalmente esto:

```c
for (size_t mix_idx = 0; mix_idx < MAX_AUDIO_MIXES; mix_idx++) {
    for (size_t ch = 0; ch < channels; ch++) {
        register float *mix = mixes[mix_idx].data[ch];
        register float *aud = source->audio_output_buf[mix_idx][ch];
        ...
        while (aud < end) *(mix++) += *(aud++);
    }
}
```

Tres hechos que se leen directamente ahí:

1. **Sumar es `+=`, y nada más.** No hay ponderación ni normalización por número
   de fuentes. Diez fuentes al 100 % suenan diez veces más fuerte, y eso es
   correcto: normalizar por cuenta haría que subir una pista bajase las demás.
2. **No hay recorte ni limitador en la mezcla.** La suma vive en float y puede
   pasar de 1,0 tranquilamente. El recorte ocurre **una sola vez**, al convertir
   a un formato de salida — que es exactamente lo que hace la conversión del
   plan 011 al saturar.
3. **Se trabaja por bloques de tamaño fijo** (`AUDIO_OUTPUT_FRAMES`), no muestra
   a muestra.

El volumen por fuente **no se aplica aquí**: llega ya aplicado, aguas arriba, en
el render de cada fuente. En la ruta de mezcla que se ve en `mix_audio()` no hay
interpolación de ganancia de ningún tipo.

### El detalle que no se hereda: la ganancia escalonada

Si la ganancia de una pista cambia de golpe entre un bloque y el siguiente, la
onda tiene una **discontinuidad**: un salto vertical en la señal. Un salto es,
por definición, energía en todas las frecuencias — se oye como un clic. A 48 kHz
con bloques de 1024 muestras, mover un fader produce un clic cada 21 ms mientras
se mueve.

La solución es estándar en cualquier consola digital: **interpolar la ganancia a
lo largo del bloque** en vez de aplicarla como escalar. Cuesta un
multiplicar-acumular por muestra en lugar de un multiplicar, y elimina el
problema por completo.

Esto es una decisión propia, no algo copiado: **no se verificó** si libobs
interpola en la ruta aguas arriba donde aplica el volumen. Lo que sí se verificó
es que en `mix_audio()` no hay interpolación.

## 5. Medidores y balance en libobs

Fuentes:
[`libobs/obs-audio-controls.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-audio-controls.c)
y [`libobs/obs-audio-controls.h`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-audio-controls.h).

### El medidor

`obs_volmeter` calcula **dos cosas por bloque y por canal**:

- **Pico**, de dos maneras según `enum obs_peak_meter_type`:
  `SAMPLE_PEAK_METER`, que es el máximo del valor absoluto de las muestras, y
  `TRUE_PEAK_METER`, que sobremuestrea ×5 con interpolación de
  Whittaker-Shannon sobre cuatro muestras para encontrar picos que caen *entre*
  muestras.
- **Magnitud**, que es la RMS: `sqrt(sum(sample²) / n)`.

Ambos se convierten a decibelios y se ajustan por el volumen del usuario.

**El hallazgo que marca el diseño: la balística no está aquí.** No hay
constantes de caída ni de retención en `obs-audio-controls.c`; el campo
`update_ms` de la estructura ni siquiera se usa. La caída, la retención de pico
y el suavizado que se ven en OBS viven en su **interfaz**, no en el núcleo.

Es la separación correcta y la adoptamos: el hilo de audio calcula números
crudos y baratos; cómo se muestran —con qué caída, cuánto se retiene el pico— es
política de presentación, y meterla en el camino de tiempo real sería poner
decisiones de interfaz donde no puede haberlas.

### Los faders

`enum obs_fader_type` da tres curvas para mapear la posición de un fader a
decibelios: `OBS_FADER_CUBIC` (x³), `OBS_FADER_IEC` (IEC 60-268-18, por tramos)
y `OBS_FADER_LOG`. Son curvas de **interfaz**, no de proceso: el mezclador
siempre recibe una ganancia lineal.

### El balance está infraespecificado

La API pública es `obs_source_set_balance_value(obs_source_t *, float balance)`,
y su documentación entera es *"Sets the balance value for a stereo audio
source"*. **Ni el rango ni la ley están documentados** en las cabeceras
públicas, y no se encontró un `enum obs_balance_type` en las dos que se leyeron.

Aquí no se copia lo que no se ha podido leer: la ley se elige, se documenta y se
justifica en el plan 013.

## 6. Remuestreo en libobs

Fuente: [`libobs/media-io/audio-resampler-ffmpeg.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/media-io/audio-resampler-ffmpeg.c).

libobs **no remuestrea: delega**. Su `audio_resampler` es una envoltura fina
sobre `SwrContext` de libswresample (FFmpeg). Se configura con
`swr_alloc_set_opts2` —frecuencias, formatos y disposiciones de origen y
destino—, se abre con `swr_init`, y cada bloque pasa por `swr_convert`, que
**devuelve cuántas muestras produjo de verdad**.

No se le pasa **ningún ajuste de calidad**: se usan los valores por omisión de
swresample. Lo único que se configura a mano es una matriz de mezcla para subir
mono a multicanal.

### El detalle que marca el diseño: la latencia se reporta

```c
*ts_offset = (uint64_t)swr_get_delay(context, 1000000000);
```

Un remuestreador tiene un filtro, y un filtro tiene retardo de grupo. libobs lo
pide en nanosegundos y lo aplica como **desfase de la marca de tiempo** de la
fuente. Sin eso, remuestrear desincroniza el audio del vídeo en una cantidad
fija y silenciosa — y como es constante, no se nota como deriva sino como un
labio que nunca cuadra.

Es lo que se adopta: **el remuestreador declara su latencia** y quien lo usa
corrige el tiempo. CLAUDE.md §5 ya dice que el audio manda en la sincronía; un
audio que llega tarde sin decirlo rompe justo eso.

### Dos cosas que no se heredan

1. **No se trae FFmpeg.** El plan 009 ya evitó esa dependencia para la salida, y
   traerla aquí significaría un árbol de bibliotecas nativas para convertir
   44 100 en 48 000. El remuestreo de tasa fija es un filtro polifásico
   bien entendido y cabe en unos cientos de líneas verificables.
2. **La razón es racional, no un `double`.** 48 000/44 100 es exactamente
   160/147. Guardarlo como 1,08843537… y acumularlo por bloque es la misma
   clase de error que el plan 002 evitó con el reloj de vídeo y el plan 011 con
   el de audio.

## 7. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:** float de 32 bits planar como formato interno único; el
catálogo de disposiciones de altavoces de `speaker_layout`, para que una escena
de OBS signifique lo mismo aquí; y la idea de que el borde convierte y el núcleo
no.

**Qué hacemos distinto:**

1. **El formato interno no es un valor de enum, es el tipo.** En libobs
   `audio_data` puede llevar cualquiera de los nueve formatos y cada consumidor
   comprueba cuál es. Aquí el búfer interno **es** float planar por
   construcción: no hay campo que consultar ni caso que olvidar. Los formatos de
   borde viven en un tipo distinto, usado solo al convertir.
2. **Profundidad y disposición son dos ejes, no nueve valores.** `AUDIO_FORMAT_*`
   mezcla "cuántos bits" con "intercalado o planar", así que responder
   *¿es planar?* obliga a enumerar la mitad de la lista, y añadir una
   profundidad duplica las entradas. Separarlos hace la pregunta trivial y el
   crecimiento lineal.
3. **El número de canales sale de la disposición.** En libobs son dos campos que
   pueden contradecirse. Una disposición de altavoces ya dice cuántos canales
   tiene; guardar el número aparte es invitar a que no cuadren.
