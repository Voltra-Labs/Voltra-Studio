# Estado del arte: medidores de nivel

Referencia para el plan 013. Cubre cómo se mide el nivel de una señal —pico,
pico verdadero, RMS—, cómo llega esa medida del hilo de audio a la interfaz, y
cómo la interfaz la convierte en una barra que se mueve (la *balística*). Se
lee el código de libobs y de la interfaz de OBS, y la norma que define el pico
verdadero.

Fuentes:

- [`libobs/obs-audio-controls.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-audio-controls.c) — `obs_volmeter`: `get_sample_peak`, `get_true_peak`, `volmeter_process_magnitude`, `volmeter_source_data_received`, `signal_levels_updated`.
- [`frontend/components/VolumeMeter.cpp`](https://github.com/obsproject/obs-studio/blob/master/frontend/components/VolumeMeter.cpp) — `setLevels`, `calculateBallisticsForChannel`, las constantes de la balística.
- [ITU-R BS.1770-5 (11/2023)](https://www.itu.int/rec/R-REC-BS.1770), anexo 2: *Guidelines for accurate measurement of "true-peak" level*, y su adjunto informativo de la AES.

Todo leído en noviembre de 2026, rama `master` de OBS.

---

## 1. Qué mide libobs

`obs_volmeter` se engancha a una fuente con `obs_source_add_audio_capture_callback`
y recibe cada `audio_data` que la fuente entrega. Por cada uno calcula, canal a
canal:

- **Pico** (`peak`), de dos maneras según `peak_meter_type`:
  - `SAMPLE_PEAK_METER`: el mayor valor absoluto de las muestras.
  - `TRUE_PEAK_METER`: además, valores interpolados entre muestras (§3).
- **Magnitud** (`magnitude`): el RMS del bloque recibido, sin ventana propia:

  ```c
  float sum = 0.0;
  for (size_t i = 0; i < nr_samples; i++)
      sum += sample * sample;
  volmeter->magnitude[channel_nr] = sqrtf(sum / nr_samples);
  ```

  El RMS cubre lo que la fuente haya entregado esa vez, así que su ventana
  depende del tamaño de bloque de cada fuente. La integración temporal de
  verdad ocurre después, en la interfaz (§4).

Y entrega tres arrays a los oyentes, en dB:

- `magnitude` y `peak` **multiplicados por el volumen** de la fuente
  (`db_to_mul(volmeter->cur_db)`): lo que se oye después del fader.
- `input_peak` **sin** el volumen, "para que el usuario pueda comprobar la
  ganancia de entrada".

Dos detalles de esa corrección por volumen: se aplica el volumen actual, no el
que tuvo el bloque, y **no incluye el balance**. Un canal llevado a un lado con
el balance sigue marcando a pleno.

No hay medida de sonoridad: el volmeter no implementa LUFS (BS.1770 §1,
ponderación K y *gating*). Solo pico y RMS.

## 2. Cómo llega al hilo de la interfaz

Todo lo anterior ocurre **en el hilo de audio**, dentro de `volmeter->mutex`. Al
terminar, `signal_levels_updated` toma `volmeter->callback_mutex` y llama a cada
oyente **de forma síncrona, desde el hilo de audio**:

```c
pthread_mutex_lock(&volmeter->callback_mutex);
for (size_t i = volmeter->callbacks.num; i > 0; i--) {
    struct meter_cb cb = volmeter->callbacks.array[i - 1];
    cb.callback(cb.param, magnitude, peak, input_peak);
}
pthread_mutex_unlock(&volmeter->callback_mutex);
```

El oyente de la interfaz, `VolumeMeter::setLevels`, toma a su vez
`dataMutex` —el mismo que toma `paintEvent` mientras dibuja— y además ejecuta
la balística entera (`calculateBallistics`) ahí mismo, en el hilo de audio.

Así que por cada bloque y cada medidor el hilo de audio toma **tres mutex**, el
último compartido con el pintado de un widget de Qt. Si la interfaz está
pintando, el audio espera a que termine de pintar.

La razón de ese diseño es buena: llamando por cada bloque **no se pierde ningún
pico**. Si la interfaz solo leyera el último valor a 60 Hz, un transitorio que
cayera en un bloque entre dos lecturas desaparecería. La balística se ejecuta
en cada bloque precisamente para que el ataque inmediato del pico lo capture.

## 3. El pico verdadero

### 3.1 Qué es y por qué importa

BS.1770, anexo 2: *el pico verdadero es el valor máximo de la forma de onda en
el dominio del tiempo continuo; puede ser mayor que la mayor muestra.* El
adjunto informativo de la AES lo cuantifica:

- Un tono a un cuarto de la frecuencia de muestreo, con la fase desafortunada,
  **se lee 3 dB por debajo** en un medidor de pico por muestra.
- Una señal puede saturar en procesos posteriores —un conversor D/A, un
  remuestreo, **un codificador**— sin que ninguna muestra esté cerca del fondo
  de escala.
- El error en el peor caso para un sobremuestreo de factor *n* y una frecuencia
  normalizada máxima *f* es `20·log10(cos(π·f/n))`. A 48 kHz, ×4 y 20 kHz
  (*f* = 0,417): **−0,47 dB**.

Para un programa que acaba en un codificador con pérdidas, el pico por muestra
no basta: es exactamente el caso de "saturar en un proceso posterior".

### 3.2 El algoritmo de la norma

Cinco etapas: atenuar 12,04 dB, sobremuestrear ×4, filtro paso bajo, valor
absoluto, pasar a dB TP. La atenuación existe para dar margen a aritmética
entera, y la norma dice expresamente que **sobra en coma flotante**.

El filtro de ejemplo es un FIR interpolador de **orden 48 en 4 fases**, doce
coeficientes por fase. Copiados de la norma:

| k | Fase 0 | Fase 1 | Fase 2 | Fase 3 |
|---|---|---|---|---|
| 0 | 0.0017089843750 | −0.0291748046875 | −0.0189208984375 | −0.0083007812500 |
| 1 | 0.0109863281250 | 0.0292968750000 | 0.0330810546875 | 0.0148925781250 |
| 2 | −0.0196533203125 | −0.0517578125000 | −0.0582275390625 | −0.0266113281250 |
| 3 | 0.0332031250000 | 0.0891113281250 | 0.1015625000000 | 0.0476074218750 |
| 4 | −0.0594482421875 | −0.1665039062500 | −0.2003173828125 | −0.1022949218750 |
| 5 | 0.1373291015625 | 0.4650878906250 | 0.7797851562500 | 0.9721679687500 |
| 6 | 0.9721679687500 | 0.7797851562500 | 0.4650878906250 | 0.1373291015625 |
| 7 | −0.1022949218750 | −0.2003173828125 | −0.1665039062500 | −0.0594482421875 |
| 8 | 0.0476074218750 | 0.1015625000000 | 0.0891113281250 | 0.0332031250000 |
| 9 | −0.0266113281250 | −0.0582275390625 | −0.0517578125000 | −0.0196533203125 |
| 10 | 0.0148925781250 | 0.0330810546875 | 0.0292968750000 | 0.0109863281250 |
| 11 | −0.0083007812500 | −0.0189208984375 | −0.0291748046875 | 0.0017089843750 |

Dos propiedades que se ven en la tabla: la fase 3 es la fase 0 invertida y la 2
es la 1 invertida, así que las cuatro fases caen en posiciones simétricas
entre muestras —a 1/8, 3/8, 5/8 y 7/8 de muestra, porque un FIR simétrico de
48 coeficientes tiene su centro entre dos de ellos—; y **ninguna fase
reproduce la muestra original** (ninguna
columna es un impulso), de modo que la salida del filtro puede quedarse
ligeramente por debajo de una muestra que esté justo en el pico.

Medido fuera del proyecto, en `f64`, con tonos de 100 Hz a 20 kHz a 48 kHz y
varias fases: el filtro lee como mucho **0,11 dB por debajo** (20 kHz) y
**0,17 dB por encima** (5 kHz) de la amplitud real. El exceso es el rizado de
su banda pasante; la ganancia en continua de las fases 0 y 3 es 1,0016
(+0,014 dB) y la de las 1 y 2, 0,973. No es un filtro plano, y un medidor que
lo use lee décimas de dB por encima en algunos tonos. Para un pico verdadero,
leer por encima es el error del lado seguro.

### 3.3 Lo que hace libobs

`get_true_peak` no usa ese filtro. Interpola con **cuatro coeficientes**
(Whittaker-Shannon sobre cuatro muestras) cuatro puntos entre cada par de
muestras —el comentario lo llama "sobremuestreo ×5"—, e incluye las muestras
originales en el máximo. Es mucho más barato que el FIR de 48 coeficientes y
menos exacto: una sinc truncada a cuatro muestras tiene bastante más error de
interpolación que doce por fase.

Además:

- **Es SSE explícito** (`_mm_load_ps`, `_mm_shuffle_ps`), y exige planos
  alineados a 16 bytes. Con un plano desalineado, `volmeter_process_peak`
  imprime con `printf` **desde el hilo de audio** y marca el pico a **1,0**
  —fondo de escala— en lugar de medirlo.
- El bucle avanza de cuatro en cuatro (`(i + 3) < nr_samples`): **las muestras
  que sobran al final de un bloque que no es múltiplo de cuatro no se miden.**
  Con bloques de 1024 no ocurre; con fuentes que entregan tamaños arbitrarios,
  sí.
- Guarda las cuatro últimas muestras de cada canal (`prev_samples`) para
  interpolar a través del borde entre bloques. Es correcto, y es lo que hay que
  copiar.

## 4. La balística, en la interfaz

`VolumeMeter` repinta con un `QTimer` de 16 ms y aplica, con las constantes por
defecto:

| Elemento | Valor | Comentario en el código |
|---|---|---|
| Caída del pico | 11,76 dB/s | "20 dB / 1.7 sec" (perfil medio, PPM tipo I) |
| Retención del pico | 20 s | vuelve al pico tras 20 s |
| Retención del pico de entrada | 1 s | |
| Integración del RMS | 300 ms | "99% in 300 ms" |
| Mínimo de la escala | −60 dB | |
| Destello de saturación | 1 s | `CLIP_FLASH_DURATION_MS` |

El ataque del pico es inmediato y la caída es lineal en dB, que es lo habitual
en un PPM. Lo interesante es el RMS:

```cpp
// A VU meter will integrate to the new value to 99% in 300 ms.
// The calculation here is very simplified and is more accurate
// with higher frame-rate.
float attack = float((currentMagnitude[channelNr] - displayMagnitude[channelNr]) *
                     (timeSinceLastRedraw / magnitudeIntegrationTime) * 0.99);
```

Tres problemas, que el propio comentario reconoce en parte:

1. **Integra en dB**, no en potencia. Promediar decibelios no es promediar
   energía: una señal que alterna entre −6 y −40 dB no se integra a lo mismo
   que su potencia media.
2. **Es Euler explícito**, con el factor `dt / 0,3 · 0,99`. Para `dt` pequeño
   se parece a una exponencial; a partir de `dt` ≈ 0,3 s el factor pasa de 1 y
   **cada paso sobrepasa el objetivo**; y a partir de 0,61 s pasa de 2 y el
   error crece en vez de menguar: el filtro diverge, y solo el `std::clamp` a
   `[−60, 0]` lo mantiene acotado, rebotando entre los extremos. Un repintado
   que se retrase medio segundo —una ventana minimizada, un sistema cargado—
   basta para que la barra del RMS se pase de largo.
3. **Depende del reloj de pared**: `os_gettime_ns()` en el hilo de audio para
   el momento del dato, y otra vez en el pintado. Es coherente, pero hace que
   la balística no se pueda probar sin dormir hilos.

---

## 5. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:**

- Pico y RMS por canal; pico verdadero opcional, porque cuesta.
- Medir **antes y después** del fader: el pico de entrada para ajustar la
  ganancia, el de salida para lo que se oye.
- Conservar las últimas muestras de cada canal para interpolar el pico
  verdadero a través del borde entre bloques.
- Las constantes de balística de OBS como valores por defecto —11,76 dB/s,
  20 s de retención, 300 ms de integración—, para que un usuario que viene de
  OBS vea moverse las barras igual.

**Qué hacemos distinto:**

1. **Ningún pico se pierde y el audio no toma ningún mutex.** En vez de llamar
   a la interfaz desde el hilo de audio, el medidor *acumula* en atómicos lo
   ocurrido desde la última lectura: el pico con `fetch_max`, la energía y el
   número de muestras empaquetados en una sola palabra. La interfaz lee y pone
   a cero cuando quiere. Si tarda tres bloques, ve el máximo de los tres; si
   tarda un minuto, también. Es la garantía de OBS sin su coste.
2. **El pico verdadero usa el filtro de la norma**, no una sinc de cuatro
   coeficientes, y en Rust portable con bucles que autovectorizan, sin
   intrínsecos de una arquitectura ni requisito de alineación. Las muestras
   originales entran también en el máximo, como en libobs, para que el pico
   verdadero nunca quede por debajo del pico por muestra (§3.2: ninguna fase
   del filtro reproduce la muestra).
3. **Se miden todas las muestras**, sea cual sea el tamaño del bloque.
4. **El nivel después del fader incluye el balance y el paneo**: se deriva de
   la ganancia de cada canal que aplica el mezclador, no de un volumen global.
5. **La medida por pista no cuesta una pasada más.** El mezclador ya lee cada
   bloque entero para descartar NaN (plan 012). La misma pasada puede calcular
   el pico y la energía: una suma de cuadrados no finita delata el NaN igual
   que la prueba actual.
6. **El RMS se integra en potencia, con la exponencial exacta.**
   `y ← y·e^(−dt/τ) + (1 − e^(−dt/τ))·x` es estable para cualquier `dt`: un
   repintado tardío llega antes al valor, pero nunca lo sobrepasa.
7. **La balística es una función pura del tiempo que se le pasa.** Se prueba sin
   relojes ni hilos, y quien la llama decide qué reloj usa.

**Qué queda fuera, a sabiendas:** sonoridad (LUFS, BS.1770 §1–2, EBU R128),
que es un medidor distinto con su propio plan; y cómo se dibuja la barra
—escalas, colores, transformaciones de deflexión—, que es de la interfaz.
