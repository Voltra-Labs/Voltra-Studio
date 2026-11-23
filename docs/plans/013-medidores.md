# 013 — Medidores de nivel

- **Fase:** 3
- **Estado:** completado (2026-11-22) — ver Resultado

## Objetivo

Que cada pista del mezclador y la mezcla final tengan medidor —pico, pico
verdadero opcional y RMS, por canal—, que la interfaz pueda leerlo desde otro
hilo **sin perder ningún pico y sin que el audio tome un mutex**, y que esa
lectura se convierta en una barra con la balística de OBS mediante una función
pura y estable. El mezclador mide sus pistas en la misma pasada que ya hace
para descartar NaN, así que medir no le cuesta una pasada más.

## Fuera de alcance

- **Sonoridad (LUFS, EBU R128).** Ponderación K, ventanas de 400 ms y 3 s,
  *gating*: otro medidor, con su propio plan.
- **Dibujar el medidor.** Escalas, colores, transformación dB → deflexión
  (OBS tiene cúbica, IEC y logarítmica): es de la interfaz, fase 7.
- **Medidores de mezclas múltiples.** Sigue habiendo una mezcla por mezclador
  (plan 012).
- **Medidor de pico verdadero por pista.** Cuesta (§Diseño) y lo que importa
  saturar o no es la salida. El `Meter` independiente se puede poner en
  cualquier señal si un caso lo pide.
- **Remuestreo y sincronía A/V**, que siguen siendo los pasos restantes de la
  fase.

## Referencia

`docs/references/audio-meters.md`, escrito para este plan a partir de
`obs-audio-controls.c`, `VolumeMeter.cpp` y la norma ITU-R BS.1770-5, anexo 2.

**Qué adoptamos:** pico y RMS por canal con pico verdadero opcional; medida
antes y después del fader; las últimas muestras de cada canal guardadas para
interpolar a través del borde entre bloques; y las constantes de balística de
OBS como valores por defecto (11,76 dB/s de caída, 20 s de retención, 300 ms de
integración del RMS).

**Qué mejoramos**, cada punto con su sección en la referencia:

1. **Ni mutex ni picos perdidos** (§2). libobs llama a la interfaz desde el hilo
   de audio bajo tres mutex, el último compartido con el pintado de Qt. Aquí el
   audio acumula en atómicos y la interfaz recoge cuando quiere: si tarda tres
   bloques ve el máximo de los tres.
2. **El filtro de la norma** para el pico verdadero (§3.2–3.3): 48
   coeficientes en 4 fases, en vez de la sinc de cuatro de libobs, en Rust
   portable sin intrínsecos ni requisito de alineación.
3. **Todas las muestras medidas** (§3.3): libobs se salta el resto de un bloque
   que no es múltiplo de cuatro.
4. **El nivel tras el fader incluye balance y paneo** (§1): se deriva de la
   ganancia de cada canal del mezclador, no del volumen global.
5. **Medir por pista sin pasada extra** (§5.5): la suma de cuadrados detecta el
   NaN igual que la prueba de plan 012, y la sustituye.
6. **RMS integrado en potencia con la exponencial exacta** (§4): estable para
   cualquier intervalo; la de OBS integra en dB y diverge con repintados de más
   de 0,6 s.
7. **Balística pura**, función del tiempo transcurrido que se le pasa (§4.3).

**Qué nos apartamos de la norma, y por qué:** el máximo incluye también las
muestras originales, como libobs. Ninguna fase del filtro de BS.1770 reproduce
la muestra, así que sin esto el "pico verdadero" de una muestra que cae justo
en el pico podría quedar por debajo del pico por muestra, que es una lectura
que nadie entendería. La atenuación de 12,04 dB se omite porque la norma dice
que sobra en coma flotante.

## Diseño

Todo en `voltra-audio`, en dos módulos nuevos: `meter/` (medida y lectura) y
`ballistics.rs` (vista). Sin dependencias nuevas, sin `unsafe`.

### La celda compartida

```rust
/// Levels accumulated since the last read, one per channel. Shared by one
/// writer (the audio thread) and one reader.
struct LevelCell {
    peak: AtomicU32,        // f32 bits, fetch_max
    true_peak: AtomicU32,   // f32 bits, fetch_max
    power: AtomicU64,       // f32 energy (high 32) | u32 frames (low 32)
}
```

- **El pico con `fetch_max` sobre los bits.** Para `f32` no negativos y no NaN,
  el orden de los bits como `u32` es el orden de los valores, así que
  `fetch_max` es un máximo de floats exacto y sin espera. El pico se calcula
  con `max`, que descarta NaN, así que a la celda no llega ninguno.
- **Energía y número de muestras en una sola palabra**, para que la lectura no
  pueda ver la energía de un bloque sin su duración. Escribir es un
  `fetch_update` (CAS): solo puede fallar si el lector acaba de leer, así que
  reintenta como mucho una vez por lectura. Libre de bloqueos aunque no
  estrictamente sin espera, y documentado así.
- **Leer es `swap(0)`** en cada atómico: devuelve lo acumulado y empieza de
  nuevo.
- El contador de muestras satura en `u32::MAX` (24,8 h a 48 kHz sin que nadie
  lea): pasado eso el RMS de esa lectura se sobreestima. Un medidor que nadie
  mira en un día no es un caso, y se documenta.

### Medidor independiente

```rust
pub enum PeakMode { Sample, True }

pub struct Meter { ... }          // audio thread: owns the filter history
pub struct MeterReader { ... }    // one reader; Send, not Clone

impl Meter {
    pub fn new(layout: ChannelLayout, mode: PeakMode) -> (Meter, MeterReader);
    pub fn process(&mut self, block: &AudioBuffer) -> bool;   // false: wrong layout, ignored
}

impl MeterReader {
    pub fn take(&mut self) -> Levels;
}

pub struct Levels {
    frames: u32,                         // audio covered by this reading
    channels: [ChannelLevels; MAX_CHANNELS],
    count: usize,
}
pub struct ChannelLevels { pub peak: f32, pub true_peak: Option<f32>, pub rms: f32 }
```

`MeterReader` no es `Clone`: dos lectores se repartirían los datos y cada uno
vería la mitad. `take` devuelve `rms = sqrt(energía / muestras)` y, si no hubo
audio desde la última lectura, `frames == 0` y niveles a cero.

### Pico verdadero

Los coeficientes de BS.1770-5, anexo 2, como `const [[f32; 12]; 4]`. Por canal,
`Meter` guarda las 11 últimas muestras del bloque anterior. Para cada muestra
de entrada, las cuatro fases dan cuatro valores interpolados; el pico es el
máximo de sus valores absolutos y de las muestras mismas.

Coste: 48 multiplicaciones-suma por muestra y canal, 98 304 por bloque estéreo
de 1024. Se escribe como 48 pasadas `y += h·x[k..]` sobre un búfer de trabajo
preasignado en `Meter::new` (una por fase), que es la forma que autovectoriza
con seguridad; el bucle de ventanas deslizantes es la alternativa si mide mejor.

### Medidores de pista en el mezclador

Cada pista gana dos celdas: **entrada** (antes del fader) y **salida** (después).

En `mix()`, la pasada de `all_finite` se sustituye por una pasada de
estadísticas por canal —`max |x|` y `Σx²` con varias sumas parciales para que
vectorice—. El bloque es no finito si alguna energía lo es (un NaN o un `inf`
contaminan la suma de cuadrados). Después:

- **Entrada:** pico y energía de cada canal del bloque, tal cual.
- **Salida:** por canal de la mezcla, `pico_entrada[origen] × g` y
  `energía_entrada[origen] × g²`, con `g = max(ganancia_previa, ganancia_nueva)`
  de ese canal. Exacto con ganancia estable; durante la rampa de un bloque es
  una cota superior, que es el lado correcto para un medidor.
- **Pistas que faltan, rechazadas o con NaN:** suman muestras con energía cero
  a las dos celdas, para que el medidor caiga en vez de congelarse.

La lectura:

```rust
impl Mixer {
    /// The meter of a track, once. `None` if unknown or already taken.
    pub fn take_track_meter(&mut self, id: TrackId) -> Option<TrackMeter>;
}
pub struct TrackMeter { ... }        // Send, not Clone
impl TrackMeter {
    pub fn take(&mut self) -> TrackLevels;     // { input: Levels, output: Levels }
}
```

La celda vive en la pista aunque nadie la haya pedido: acumular en ella cuesta
dos atómicos por canal y bloque, y así el lector puede llegar tarde.

La mezcla final se mide con un `Meter` normal sobre el bloque de salida, en
`PeakMode::True`; el mezclador no lo lleva dentro, porque quien decide si la
salida necesita pico verdadero es quien la manda a un codificador.

### Balística

```rust
pub struct BallisticsConfig {
    pub peak_decay_db_per_s: f32,   // 11.76, Type I PPM, OBS default
    pub peak_hold: Duration,        // 20 s
    pub rms_integration: Duration,  // 300 ms to 99 %
}
pub struct Ballistics { ... }
impl Ballistics {
    pub fn update(&mut self, levels: &Levels, elapsed: Duration) -> Display;
}
pub struct ChannelDisplay { pub peak_db: f32, pub hold_db: f32, pub rms_db: f32, pub clipped: bool }
```

- **Pico:** ataque inmediato, caída lineal en dB a la velocidad configurada.
  Usa el pico verdadero si la lectura lo trae.
- **Retención:** el máximo de los últimos `peak_hold`.
- **RMS:** en potencia, `y ← y·a + (1 − a)·x` con `a = e^(−dt/τ)` y
  `τ = 0,3 s / ln 100`, de modo que llega al 99 % en 300 ms para cualquier
  partición del tiempo: dos pasos de 150 ms dan lo mismo que uno de 300.
- **Saturación:** `clipped` se enciende con un pico ≥ 1,0 (0 dBFS) y queda
  encendido hasta `reset_clip()`. OBS lo apaga solo al segundo; en una
  emisión en directo, una saturación que nadie vio no debería borrarse sola.
  Es una decisión de producto que se puede revertir con una línea.
- Ninguna variable de reloj dentro: `elapsed` lo pone quien llama.

## Pasos

Cuatro commits, cada uno con la puerta de calidad en verde:

1. **`LevelCell`, `Meter` (pico por muestra y RMS), `MeterReader`, `Levels`.**
   Tests de exactitud y de lectura concurrente; benchmark.
2. **Pico verdadero** con el filtro de BS.1770. Tests contra tonos de amplitud
   conocida; benchmark.
3. **Medidores de pista en el mezclador**, con la pasada de estadísticas en
   lugar de `all_finite`, y `take_track_meter`. Benchmark antes/después de
   `mix()`; ampliar el test de cero asignaciones.
4. **Balística.** Tests de las leyes con tiempos sintéticos. Al final,
   `PERFORMANCE.md`, `ROADMAP.md`, `CLAUDE.md` §8 y este plan.

## Verificación

**Celda y medidor:**

- Pico y RMS exactos sobre bloques conocidos: un seno de amplitud A da
  RMS = A/√2 (dentro de 1e-4 en un número entero de periodos); una constante c
  da pico = RMS = |c|.
- **Ningún pico se pierde:** procesar 100 bloques silenciosos con un único
  impulso en el 37 y leer una sola vez devuelve el impulso.
- Leer dos veces sin audio entre medias devuelve `frames == 0` la segunda.
- Un hilo procesa 10 000 bloques mientras otro lee en bucle: la suma de
  `frames` leídos es exactamente 10 000 × 1024 (nada se pierde ni se duplica), y
  el máximo de los picos leídos es el pico real.
- Un bloque de otra disposición se ignora y `process` devuelve `false`.

**Pico verdadero:**

- **El caso de la norma:** un tono a fs/4 con fase de 45°, amplitud 1. El pico
  por muestra lee 0,707 (−3,01 dB); el verdadero, entre −0,25 y +0,1 dB TP.
- Barrido de tonos de 100 Hz a 20 kHz y fases variadas a 48 kHz: el pico
  verdadero nunca queda más de **0,5 dB** por debajo de la amplitud real (el
  error de muestreo de ×4 en el peor caso es −0,47 dB, referencia §3.1) ni más
  de **0,25 dB** por encima.

  Estos umbrales están comprobados de antemano: el filtro de la norma,
  evaluado en `f64` fuera del proyecto con el mismo barrido, da **+0,045 dB** a
  fs/4 a 45°, **−0,11 dB** como peor lectura por debajo (20 kHz) y **+0,17 dB**
  como peor por encima (5 kHz). Que fs/4 lea por encima y no por debajo tiene
  explicación: los puntos interpolados caen a 1/8, 3/8, 5/8 y 7/8 de muestra,
  el pico de ese tono está en 1/2 y la distancia de 1/8 resta 0,17 dB, pero el
  rizado del filtro a 12 kHz suma unos 0,21. El filtro de la norma no es plano,
  y el test no lo finge.
- Nunca por debajo del pico por muestra.
- Un tono que cruza el borde entre dos bloques mide lo mismo que el mismo tono
  en un único bloque (la historia de 11 muestras funciona).
- Bloques de tamaño no múltiplo de cuatro: se miden todas las muestras.

**Mezclador:**

- La medida de entrada de una pista es la de un `Meter` sobre su bloque.
- Con ganancia 0,5, el pico de salida es la mitad del de entrada; con balance a
  la izquierda, el canal derecho de salida marca cero; una pista mono con paneo
  al centro marca 0,707 × su pico en cada lado.
- Una pista que falta, se rechaza o trae NaN hace caer sus medidores, no los
  congela.
- `take_track_meter` da el lector una vez; la segunda, `None`.
- Los tests de aislamiento del plan 012 siguen en verde: la nueva pasada
  detecta NaN, `+inf` y `−inf`.
- Cero asignaciones, con los medidores activos y leídos desde otro hilo.

**Balística:**

- El pico cae 11,76 dB en 1 s, en un paso o en cien.
- La retención aguanta 20 s y luego vuelve al pico.
- El RMS de un escalón llega al 99 % en 300 ms, y un paso de 300 ms da lo mismo
  que diez de 30 ms (dentro de 1e-5). Un paso de 5 s no sobrepasa el objetivo.
- Un pico de 1,0 enciende `clipped`; `reset_clip` lo apaga.

**Rendimiento**, por bloque de 1024 muestras a 48 kHz, en `docs/PERFORMANCE.md`:

| Caso | Criterio |
|---|---|
| `Meter` estéreo, pico por muestra | Anotar |
| `Meter` estéreo, pico verdadero | **< 1 % del bloque (213 µs)**; estimación 15–30 µs |
| `mix()` 8 pistas estéreo estable, con medidores | **≤ 3,4 µs**, el +25 % sobre los 2,70 µs del plan 012 |
| `mix()` 8 pistas en rampa, con medidores | Anotar frente a los 3,24 µs del plan 012 |

El criterio del mezclador es el importante: medir sustituye a la pasada de NaN,
así que el coste debería quedarse casi igual. Si sube más de un 25 %, la fusión
no ha funcionado y se investiga antes de seguir.

## Riesgos

- **`max` no vectoriza.** `f32::max` tiene semántica de NaN que a veces impide
  usar la instrucción vectorial. Plan B: reducción por carriles con
  comparación explícita (`if x > m`), o el máximo de `|x|` sobre los bits como
  enteros. Se decide con el benchmark del paso 1.
- **El filtro de pico verdadero cuesta más de lo estimado.** Si pasa del 1 %
  del bloque, se mide la forma de ventanas deslizantes; si sigue caro, se
  queda opcional y fuera del camino por defecto, que es como está diseñado.
- **El rizado del filtro sobrepasa el margen en `f32`.** Medido en `f64`, el
  peor exceso es +0,17 dB frente a un margen de 0,25. Si la implementación en
  `f32` lo rebasa, se documenta la cifra y se ajusta el margen a ella; no se
  cambia el filtro de la norma sin un motivo medido.
- **Derivar la salida del fader con `max(g0, g1)`** sobreestima durante una
  rampa. Es un bloque de 21 ms y en el sentido seguro; si se ve en la interfaz,
  la alternativa es la media de `g²` sobre la rampa, que se calcula en cerrado.
- **El CAS de la energía reintenta.** Con un solo lector, como mucho una vez por
  lectura. El test concurrente lo ejercita; si algún día hubiera lectores a
  frecuencia altísima, el diseño correcto es una cola SPSC, no más CAS.

## Resultado

Puerta de calidad en verde: **260 tests** (desde 223), `clippy -D warnings`
limpio, `fmt` limpio. Cuatro commits, uno por paso: `e018557` (medidores),
`6ea9ff7` (pico verdadero), `24b82f1` (medidores de pista) y el de la
balística, que cierra el plan.

### Lo que quedó hecho

En `voltra-audio`: `Meter`/`MeterReader` con `PeakMode::{Sample, True}`,
`Levels`/`ChannelLevels`; el filtro de BS.1770-5 anexo 2 con historia entre
bloques; `Mixer::take_track_meter` con `TrackMeter`/`TrackLevels`, entrada y
salida del fader; y `Ballistics` con `BallisticsConfig::OBS`, `MeterDisplay` y
`ChannelDisplay`. Sin dependencias nuevas y sin `unsafe` fuera del asignador
contador de los tests.

### Las cifras

En `docs/PERFORMANCE.md` §2.

- **Pico verdadero: 9,0 µs** por bloque estéreo, el 0,042 % del bloque, frente
  a un umbral de 213 µs y una estimación de 15–30 µs.
- **`mix()` con medidores: 3,25 µs** para 8 pistas estables, **+23 %** sobre la
  línea base de la misma sesión, dentro del +25 %. La primera versión daba
  +50 %; el plan decía investigar antes de seguir, y se investigó: 16 carriles
  en vez de 8 en la pasada de pico y energía, y sin la atómica de pico
  verdadero en las pistas. Los cuatro experimentos, los dos que salieron y los
  dos que no, están en `PERFORMANCE.md`.
- **Exactitud del pico verdadero en `f32`**, tonos de 100 Hz a 20 kHz: entre
  **−0,108 y +0,171 dB**, idéntico al estudio previo en `f64`. El filtro solo,
  sin tomar también las muestras, llega a −0,197 dB: la decisión de incluirlas,
  que el plan justificaba en teoría, queda respaldada por una cifra.

### Dos fallos que encontraron los tests

- **La duración de una lectura se contaba doble.** `Levels::frames` salía del
  máximo de los contadores por canal; una lectura que cae entre el canal 0 y el
  1 de un bloque lo cuenta en dos lecturas. El test concurrente lo vio a la
  primera: 20,4 millones de muestras leídas para 10,2 millones escritas. Ahora
  la duración es un contador único del medidor, sumado una vez por bloque; el
  RMS de cada canal sigue usando su par energía/muestras empaquetado.
- **El contador de asignaciones del plan 012 era compartido entre hilos.** Con
  dos tests armados a la vez, cada uno sumaba las asignaciones del otro. Con un
  solo test armado nunca se notó; al añadir el segundo, falló de forma
  intermitente. Ahora el recuento es por hilo. No había ninguna asignación real:
  con el contador corregido, ambos dan cero.

## Desviaciones respecto al plan

- **El pico verdadero usa ventanas deslizantes**, la alternativa del plan, no
  las 48 pasadas sobre un búfer de trabajo. Midió 9–10 µs, muy por debajo del
  umbral, y no necesita búfer, así que `Meter::new` no tiene que conocer el
  tamaño de bloque.
- **Una muestra al final de un bloque se interpola en el siguiente.** El filtro
  está centrado unas seis muestras atrás. No se pierde nada —el pico por
  muestra la recoge en el acto, y el test lo comprueba—, pero el plan decía
  "se miden todas las muestras" sin mencionar ese retraso de 0,125 ms.
- **La balística usa dos relojes**, no solo el `elapsed` del llamante. El pico
  y su retención siguen el tiempo de pared; el RMS integra el tiempo de audio
  de la lectura (sus muestras entre la frecuencia). Con un repintado a 60 Hz y
  bloques a 47 Hz, muchas lecturas llegan vacías, y tratarlas como silencio
  haría parpadear la barra. Por eso `Ballistics::new` recibe la frecuencia de
  muestreo.
- **`Levels::frames` es `u64`**, no `u32`: es el contador del medidor entero, y
  no tiene por qué saturar.
- **El "99 % en 300 ms" se mide en potencia**, no en dB como en OBS: es lo que
  significa integrar potencia.
- **La pasada de estadísticas tiene 16 carriles**, no los 8 de `all_finite`:
  medido (ver Resultado).
- **Un bloque cuya energía desborda `f32`** —muestras de más de ~10¹⁹— cuenta
  como no finito. Consecuencia de detectar el NaN por la suma de cuadrados;
  ninguna señal real se acerca, y está documentado en `Mixer::mix`.
- **El techo del +25 % se cumple en 8 pistas, no en 64**, que suben un +32 %
  (20,6 → 26,6 µs, el 0,12 % del bloque). El criterio se escribió para 8; se
  anota para que nadie lo descubra después.
