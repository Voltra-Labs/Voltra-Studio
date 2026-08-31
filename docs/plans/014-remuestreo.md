# 014 — Remuestreo

- **Fase:** 3
- **Estado:** completado (2026-08-31) — ver Resultado

## Objetivo

Que una fuente a 44 100 Hz pueda llegar a un mezclador a 48 000 Hz. Un
remuestreador polifásico de razón fija, con la razón como fracción exacta, sin
asignaciones en el camino de datos, y que **declara su latencia** para que la
sincronía A/V pueda corregirla.

Hoy el mezclador rechaza una pista con la frecuencia equivocada (plan 012); al
terminar esto, se le puede dar una que encaje.

## Fuera de alcance

- **Remuestreo de razón variable.** Ajustar la razón sobre la marcha es lo que
  hace falta para corregir la deriva entre dos relojes de hardware distintos
  (una tarjeta a 48 000,3 Hz reales). Es un problema de la fase de captura, con
  su propio lazo de control, y llega con ella.
- **Conversión de disposición de canales.** Mono a estéreo, 5.1 a estéreo. Es
  una matriz, no un filtro, y merece su propio paso.
- **Presets de calidad.** Un solo filtro, dimensionado y medido. libobs no
  configura ninguno tampoco.
- **La corrección de la latencia.** Aquí se *declara*; aplicarla a las marcas de
  tiempo es el plan de sincronía A/V, que es el que cierra la fase.

## Referencia

`docs/references/audio.md` §6, escrito para este plan.

libobs no remuestrea: envuelve `SwrContext` de libswresample, sin pasarle ningún
ajuste de calidad, y —lo importante— pide la latencia con
`swr_get_delay(context, 1000000000)` para aplicarla como desfase de marca de
tiempo.

**Qué adoptamos:** que el remuestreador **declare su latencia**, y que el
procesado devuelva **cuántas muestras produjo de verdad** en vez de que el
llamante lo calcule.

**Qué mejoramos:**

1. **Sin FFmpeg.** El plan 009 ya evitó esa dependencia para la salida; traerla
   para convertir 44 100 en 48 000 sería un árbol de bibliotecas nativas por un
   filtro que cabe en unos cientos de líneas verificables. Y una dependencia
   nativa rompería el `cargo test --workspace` en contenedor headless que
   CLAUDE.md §3 exige.
2. **La razón es una fracción exacta.** 48 000/44 100 es 160/147, no
   1,08843537… Acumular un `double` por bloque es la misma clase de error que el
   plan 002 evitó en el reloj de vídeo.
3. **La latencia es exacta, no estimada.** Es el retardo de grupo del filtro, que
   se conoce en el momento de construirlo. `swr_get_delay` devuelve un retardo
   *en curso* que depende de lo que haya en la cola; aquí es una constante del
   diseño.

## Diseño

En `voltra-audio`, junto al mezclador.

### La razón

```rust
pub struct ResampleRatio { interpolation: u32, decimation: u32 }  // L/M en forma irreducible
```

De 44 100 a 48 000: `gcd(48000, 44100) = 300`, luego **L=160, M=147**. Si las dos
frecuencias son iguales, L=M=1 y el remuestreador es un paso directo.

### El filtro

Sinc enventanado con **ventana de Kaiser**, descompuesto en L fases.

- Frecuencia de corte: `0.5 / max(L, M)` en el dominio sobremuestreado. Al subir
  manda 1/(2L); al bajar manda 1/(2M), que es lo que evita el aliasing.
- Ganancia L, para compensar los ceros que introduce la interpolación.
- `TAPS` coeficientes por fase, así que L×TAPS en total. Para 160/147 con 32
  tomas son 5 120 `f32`: 20 KiB, que cabe en caché.

Los coeficientes se calculan **una vez, al construir** —hay senos y una función
de Bessel de por medio—, nunca en el camino de datos.

### El bucle

La descomposición polifásica dice que la salida *n* es

```
salida[n] = Σ_t  h[fase + t·L] · entrada[base − t]
```

con `m = n·M`, `fase = m mod L`, `base = m div L`. La fase y la base **no se
calculan con multiplicaciones crecientes**: avanzan por incrementos exactos
—`fase += M`, y cuando pasa de L se resta L y `base += 1`—, así que no hay
`u64` creciendo ni redondeo acumulado.

### Estado e historia

Cada canal necesita las `TAPS−1` muestras anteriores. Se guardan en un búfer
contiguo preasignado por canal, con la historia delante y el bloque nuevo
detrás; al terminar, la cola se copia al principio. Son 31 `f32` por canal y
bloque: menos que cualquier alternativa con anillo, y sin aritmética modular en
el bucle caliente.

### La API

```rust
pub struct Resampler { … }

impl Resampler {
    pub fn new(from: SampleRate, to: SampleRate, layout: ChannelLayout, max_frames: usize) -> Result<Self>;
    pub fn ratio(&self) -> ResampleRatio;
    /// El retardo de grupo del filtro, en muestras de salida.
    pub fn latency_frames(&self) -> usize;
    /// El mismo retardo como duración, para corregir marcas de tiempo.
    pub fn latency(&self) -> Duration;
    /// Cuántas muestras de salida producirá este bloque de entrada.
    pub fn output_frames_for(&self, input_frames: usize) -> usize;
    /// Procesa un bloque. Devuelve las muestras escritas.
    pub fn process(&mut self, input: &AudioBuffer, out: &mut AudioBuffer) -> Result<usize>;
}
```

`process` no asigna. `out` lo aporta el llamante y puede ser más grande que lo
producido, porque el número de muestras de salida por bloque **no es constante**:
con 160/147, un bloque de 1024 da unas veces 1114 y otras 1115.

## Pasos

1. `ResampleRatio` con su reducción y sus tests.
2. Diseño del filtro: Kaiser, sinc, descomposición polifásica.
3. `Resampler::process` y el estado por canal.
4. Ampliar el test de asignación cero.
5. `benches/resample.rs`.
6. Documentación, puerta de calidad y commit.

## Verificación

Lo que de verdad demuestra que un remuestreador funciona es la **calidad de la
señal**, no que devuelva el número de muestras correcto:

- **Una senoide sigue siendo esa senoide.** Un tono de 1 kHz a 44,1 kHz
  remuestreado a 48 kHz se compara contra un tono de 1 kHz generado
  analíticamente a 48 kHz, alineando la latencia declarada. Criterio: **SNR por
  encima de 60 dB**, que es más de lo que resuelve un entero de 16 bits.
- **No hay aliasing al bajar.** Un tono por encima de la nueva frecuencia de
  Nyquist tiene que **desaparecer**, no reaparecer doblado. Es el fallo que
  distingue un remuestreador de una interpolación lineal.
- **Continua a ganancia unidad.** Una señal constante de 0,5 sale como 0,5, no
  como 0,5·L ni 0,45. Fija la normalización del filtro.
- Silencio sigue siendo silencio, exactamente.
- Frecuencias iguales: la salida es la entrada **bit a bit**, por el atajo de
  razón 1:1.
- El número de muestras producidas casa con `output_frames_for`, y a lo largo de
  cien bloques la suma no deriva de `N·L/M`.
- **Cero asignaciones** en `process`, con el asignador contador del plan 012.
- Y el que cierra el círculo: **una pista a 44,1 kHz remuestreada entra en el
  mezclador a 48 kHz** que antes la rechazaba.

Criterio de aceptación: SNR > 60 dB en el test de senoide, sin aliasing, y coste
por bloque medido y anotado.

## Riesgos

- **El filtro sale mal y el test de SNR no lo detecta.** Un error de fase o una
  normalización torcida pueden dar SNR alta y sonar mal. Mitigación: el test de
  continua fija la ganancia, y el de aliasing fija la banda de rechazo; los tres
  juntos son difíciles de pasar por accidente.
- **32 tomas pueden no bastar** para 60 dB. Es un parámetro; si no llega, se sube
  y se anota el coste, que crece linealmente.
- **La tentación de `mul_add`.** El bucle interno es una convolución, que es
  literalmente multiplicar-acumular. `docs/PERFORMANCE.md` §3.7 y §3.8 dicen que
  sin FMA en hardware eso es una llamada a libm por muestra. Este plan **no usa
  `mul_add`**, y es la tercera vez que hace falta escribirlo.

## Resultado

Puerta de calidad en verde: **260 tests** (desde 242), `clippy -D warnings` y
`fmt` limpios. `voltra-audio` suma 13 tests de remuestreo, 2 de integración con
el mezclador y 1 de asignación cero.

### El criterio de aceptación

- **SNR de la senoide: por encima de 60 dB**, medido con un DFT de un solo
  bin a 1 kHz.
- **Sin aliasing donde importa**: 48 → 8 kHz con un tono de 6 kHz, que se
  doblaría a 2 kHz en medio del habla, no deja nada medible.
- **Ganancia continua exacta**: una señal constante de 0,5 sale a 0,5.
- **Frecuencias iguales: copia bit a bit**, por el atajo de razón 1:1.
- **Cero asignaciones** en `process`.
- Y el que cierra el círculo: una pista a 44,1 kHz que el mezclador **rechazaba**
  entra al remuestrearla, y su medidor lee el nivel correcto.

### Las cifras

| Operación, 1024 muestras estéreo | Coste | % del bloque |
|---|---|---|
| Remuestrear 44,1 → 48 kHz | 62,7 µs | 0,29 % |
| Remuestrear 48 → 44,1 kHz | 109 µs | 0,51 % |

Bajar cuesta 1,7× lo que subir, porque el filtro se alarga con el factor de
diezmado. Sigue siendo medio por ciento del bloque.

### Un test mío que estaba mal, otra vez

El test de SNR daba **−5,2 dB** y el remuestreador estaba bien. Comparaba muestra
a muestra contra una senoide generada aparte, alineada a mano restando la
latencia declarada — así que medía mi aritmética de alineación tanto como el
filtro, y la aritmética estaba mal.

Reescrito en el dominio de la frecuencia —cuánta potencia queda en el bin de
1 kHz frente al resto— pasa a la primera. **Un test que depende de la fase es un
test frágil cuando lo que se mide tiene retardo de grupo.** Es la segunda vez en
tres planes que el arnés falla antes que el código (plan 012, el contador de
asignaciones); en los dos casos lo barato habría sido tocar el código hasta que
el test pasara.

### Un fallo de diseño que el test de aliasing sí encontró

El prototipo se dimensionaba como `TAPS × L`. Con `L` grande —44,1 → 48 da
L = 160— sale un filtro largo y afilado gratis. Pero al diezmar mucho `L` es
pequeño: **48 → 8 kHz da L = 1, y el filtro entero eran 32 coeficientes**. No
filtraba nada.

Ahora las tomas por fase escalan con `M/L`, así que la longitud total se mantiene
en ambos sentidos. Detalle y el análisis de la banda de transición entre 44,1 y
48 kHz —que es estrecha por aritmética, no por el filtro— en
`docs/PERFORMANCE.md` §3.9.

## Desviaciones respecto al plan

- **`TAPS` dejó de ser una constante.** Es lo que arregló el diezmado grande.
- **Se añadió `CUTOFF_FACTOR`**, no previsto: una pared vertical exactamente en
  Nyquist necesita un filtro infinito, así que se cede el 10 % superior de la
  banda para comprar banda de transición. `libswresample` corta por ahí también.
- **El test de aliasing cambió de caso.** El plan lo planteaba sobre 48 → 44,1,
  donde resulta que el contenido que se dobla acaba por encima de 20 kHz y es
  inaudible; se movió a 48 → 8 kHz, donde el doblado cae en el habla. El caso
  original está analizado con números en §3.9 en vez de convertido en un test
  que exige lo que no hace falta.
- **`latency()` y `latency_frames()` difieren en una muestra** por el redondeo a
  nanosegundos enteros. Está documentado en el tipo y comprobado con tolerancia
  de uno.
