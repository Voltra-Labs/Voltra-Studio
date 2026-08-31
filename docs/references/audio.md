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

## 4. Resumen: qué adoptamos y qué mejoramos

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
