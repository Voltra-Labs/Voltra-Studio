# 012 — Mezclador de audio

- **Fase:** 3
- **Estado:** completado (2026-11-22) — ver Resultado

## Objetivo

Un `Mixer` en una crate nueva, `voltra-audio`, que suma pistas en un bloque de
salida con **volumen, balance/paneo y silencio por pista**, controlables desde
otro hilo, y que lo hace **sin asignar memoria ni tomar un bloqueo** dentro de
`mix()`. Es el criterio de salida de la fase 3, y aquí se convierte en dos
comprobaciones automáticas: un test que cuenta asignaciones y un lint que
prohíbe mutexes en la crate.

## Fuera de alcance

- **Alineación por marca de tiempo y buffering.** El `start_point` de libobs y
  su margen de ~1 s. Aquí cada pista entrega un bloque que **empieza
  exactamente** donde el de salida, o se rechaza. Es el plan de sincronía, con
  su propio modelo explícito de buffering (`obs-studio.md` §6).
- **Remuestreo y remezcla general.** Se admiten pistas con la misma disposición
  que la mezcla, y mono → estéreo. Cualquier otra combinación (5.1 → estéreo,
  estéreo → mono) se rechaza al añadir la pista, con error. Las matrices de
  remezcla van con el remuestreo.
- **Medidores.** Pico, RMS y true peak son el plan siguiente. Los contadores de
  fallos de este plan sí van, porque CLAUDE.md §4.10 no los deja para después.
- **Mezclas múltiples.** Un `Mixer` produce una mezcla. Cómo se reparten seis
  —un mezclador por mezcla o una matriz de envíos— se decide cuando haya dos
  salidas que lo pidan (`audio-mixer.md` §6.6).
- **El hilo y el callback.** No hay dispositivo hasta PipeWire (fase 5). El
  mezclador se diseña para que lo llame un callback de tiempo real, y se
  verifica como si lo fuera.
- **Añadir y quitar pistas en caliente.** Requieren `&mut Mixer`, es decir, el
  hilo dueño. Mandarlas desde la UI por una cola SPSC es trabajo del motor
  (`voltra-engine`), que todavía no existe. Quitar una pista con señal produce
  un corte; el fundido de salida va con esa cola.
- **Push-to-talk, push-to-mute, desfase de sincronía por fuente.** Son capas
  sobre la ganancia y el reloj, no sobre la suma.

## Referencia

`docs/references/audio-mixer.md`, escrito para este plan a partir del código de
libobs, más `audio.md` (representación) y `obs-studio.md` §3 (pipeline).

**Qué adoptamos:** suma en float planar sin recortar; recorte una sola vez, en
la conversión de salida (ya lo hace `write_to` del plan 011); NaN convertido en
silencio; volumen, balance y silencio por fuente como vocabulario del usuario.

**Qué mejoramos**, cada punto con su sección en la referencia:

1. **Sin mutex entre UI y audio** (§2.3). libobs toma `audio_actions_mutex` en
   el hilo de audio por cada fuente y bloque, y la UI hace `da_push_back`
   —posible `realloc`— dentro de esa sección. Aquí los parámetros son atómicos.
2. **Rampa en vez de escalón** (§2.2). Silenciar en libobs es pasar de 1 a 0 en
   una muestra: un clic. Aquí todo cambio de ganancia es una rampa lineal de un
   bloque.
3. **Balance continuo en el centro** (§3). La ley del seno de libobs vale
   −3 dB en el centro, y la banda muerta de 0,49–0,51 lo tapa a costa de un
   salto de ~3 dB al salir de ella. Aquí balance (estéreo → estéreo) y paneo
   (mono → estéreo) son leyes distintas, y el balance vale exactamente 1 en el
   centro.
4. **Un NaN aísla su pista, no silencia la mezcla** (§4). libobs limpia después
   de sumar, así que una fuente rota silencia a todas en esas muestras, y un
   `inf` pasa a fondo de escala. Aquí se comprueba por pista antes de sumar.
5. **Una pasada en vez de tres** (§1, §3). Balance plegado en la ganancia de
   cada canal, y ganancia aplicada dentro de la suma.

**Qué perdemos, a sabiendas:** la exactitud a la muestra de los cambios de
volumen de libobs. Un cambio pedido a mitad de bloque se aplica en el siguiente,
como mucho 21,3 ms después, y en forma de rampa. Para un fader movido a mano es
inaudible; para automatización exacta a la muestra hará falta una cola de
eventos con marca de tiempo, que es justo lo que este plan no quiere en el hilo
de audio sin un motivo medido.

## Diseño

### Dónde vive

Crate nueva **`voltra-audio`**, la que CLAUDE.md §2 reserva para el procesado de
audio. Depende solo de `voltra-core` (el vocabulario del plan 011) y
`thiserror`. Ninguna dependencia nueva en el workspace salvo la propia crate;
`criterion` ya está como dev-dependency en `voltra-core` y se reutiliza.

### Tipos de parámetro

```rust
/// Linear amplitude, 0.0 ..= Gain::MAX. Not dB: dB is a view.
pub struct Gain(f32);
impl Gain {
    pub const UNITY: Self;          // 1.0, 0 dB
    pub const SILENT: Self;         // 0.0
    pub const MAX: Self;            // 10.0, +20 dB
    pub fn new(linear: f32) -> Result<Self>;   // rejects NaN, inf, < 0, > MAX
    pub fn from_db(db: f32) -> Result<Self>;   // -inf dB -> SILENT
    pub fn linear(self) -> f32;
    pub fn db(self) -> f32;
}

/// Left/right position, -1.0 (left) ..= 1.0 (right), 0.0 centred.
pub struct Balance(f32);
```

`Gain` es lineal porque es lo que se multiplica; los decibelios son la vista de
la UI. El tope de +20 dB no viene de ningún referente: es el límite a partir del
cual un valor es casi seguro un error de configuración, y un `f32` sin tope
convierte ese error en una señal a fondo de escala. Se sube si alguien lo pide
con un caso.

`Balance` va de −1 a 1 y no de 0 a 1 como en libobs: el centro es 0 y el signo
dice el lado. Importar una escena de OBS es `b = 2·b_obs − 1`.

### Leyes de ganancia por canal

Cada pista se reduce, una vez por bloque, a un array de ganancias por canal de
salida, `[f32; MAX_CHANNELS]`. Es lo único que mira el bucle de suma.

| Pista → mezcla | Ley | Centro |
|---|---|---|
| Misma disposición, no estéreo | `g` en todos los canales; balance ignorado | `g` |
| Estéreo → estéreo | **Balance**: `L = g·cos(max(0, b)·π/2)`, `R = g·cos(max(0, −b)·π/2)` | `g` exacto |
| Mono → estéreo | **Paneo a potencia constante**: `θ = (b+1)·π/4`, `L = g·cos θ`, `R = g·sin θ` | `0,707·g` (−3 dB) |

El balance solo atenúa el lado contrario y es continuo en todo el rango; el
paneo cae −3 dB en el centro porque reparte una señal entre dos altavoces y la
potencia total tiene que ser constante. Silenciar es una ganancia objetivo de 0.

La trigonometría se evalúa por pista y bloque, nunca por muestra.

### Control entre hilos

```rust
pub struct TrackId(u32);

/// Handle the UI keeps. Clone + Send + Sync.
pub struct TrackControl(Arc<TrackParams>);
impl TrackControl {
    pub fn set_gain(&self, gain: Gain);
    pub fn set_balance(&self, balance: Balance);
    pub fn set_muted(&self, muted: bool);
    pub fn gain(&self) -> Gain;             // etc.
    pub fn faults(&self) -> u64;            // blocks this track lost
}

struct TrackParams {
    gain: AtomicU32,      // f32 bits
    balance: AtomicU32,   // f32 bits
    muted: AtomicBool,
    faults: AtomicU64,
}
```

`Ordering::Relaxed` en todo: cada parámetro es independiente y no publica otra
memoria. La consecuencia, documentada: si la UI cambia ganancia y balance a la
vez, el mezclador puede ver uno en un bloque y otro en el siguiente. Con rampa,
eso son dos transiciones suaves de 21 ms en vez de una; no se oye.

El `Arc` se clona al crear la pista, nunca en `mix()`. Los atómicos de 32 y
64 bits son sin bloqueo en todos los targets que nos importan, y la crate falla
al compilar (`#[cfg(not(target_has_atomic = "64"))] compile_error!`) en uno
donde no lo sean, en vez de caer en silencio a una emulación con locks.

### El mezclador

```rust
pub struct MixSpec { pub rate: SampleRate, pub layout: ChannelLayout, pub frames: usize }

pub struct Mixer {
    spec: MixSpec,
    tracks: Vec<Track>,        // sorted by TrackId; ids are never reused
    ramp: Vec<f32>,            // k / frames, built once in new()
    next_id: u32,
    totals: MixTotals,         // cumulative counters
}

struct Track {
    id: TrackId,
    layout: ChannelLayout,
    params: Arc<TrackParams>,
    applied: [f32; MAX_CHANNELS],  // per-channel gains reached last block
    seen: u64,                     // block number of last input, for duplicates
}

impl Mixer {
    pub fn new(spec: MixSpec) -> Result<Self>;
    pub fn add_track(&mut self, layout: ChannelLayout) -> Result<(TrackId, TrackControl)>;
    pub fn remove_track(&mut self, id: TrackId) -> bool;
    pub fn mix<'a, I>(&mut self, inputs: I, out: &mut AudioBuffer) -> MixReport
    where I: IntoIterator<Item = (TrackId, &'a AudioBuffer)>;
}
```

`mix` toma las entradas como iterador de pares, así que el llamante puede pasar
un array en la pila (`mixer.mix([(mic, &a), (music, &b)], &mut out)`) y no hay
`Vec` intermedio.

Flujo de `mix`, por bloque:

1. `out.silence()` (44 ns medidos en el plan 011).
2. Por cada `(id, buffer)`:
   - Buscar la pista (búsqueda binaria). Desconocida o repetida en este bloque
     → `rejected`.
   - Comprobar que coinciden frecuencia, número de frames y `pts` con `out`.
     Si no → `rejected` y `faults += 1` en la pista.
   - Comprobar que todas las muestras son finitas, con una reducción sin ramas
     (`acc += x - x`, que solo es 0 si no hay NaN ni `inf`) que vectoriza. Si
     no → `non_finite`, `faults += 1`, y la pista no suma.
   - Leer los atómicos, calcular las ganancias objetivo por canal.
   - Sumar: si objetivo == aplicada, `out += in × g`; si no,
     `out += in × (g0 + (g1 − g0) × ramp[k])`. Los dos bucles con
     `zip`/`chunks_exact`, sin indexar (CLAUDE.md §4.4).
   - Guardar el objetivo como `applied`.
3. Las pistas que no entregaron bloque cuentan como `missing`, y su `applied` se
   pone a 0: cuando vuelvan, entrarán con un fundido de un bloque en vez de un
   clic. Es el *dropout sin arrastrar al resto* de libobs, con la entrada
   suavizada.
4. Una pista nueva empieza con `applied = 0` por la misma razón.

`MixReport { mixed, missing, rejected, non_finite }` es `Copy` y sale por valor.
`totals` acumula lo mismo para las métricas del pipeline.

`out` no se recorta: puede pasar de ±1 y es correcto que lo haga. El recorte es
de `write_to` al convertir, como en libobs y como fija `audio.md` §2.

### Errores

`voltra_audio::Error` con `thiserror`: `UnsupportedLayout { track, mix }`,
`InvalidSpec(..)`, `InvalidGain(f32)`, `InvalidBalance(f32)`. Ninguno sale de
`mix()`: lo que falla en el bloque se cuenta, no se propaga. Un fallo de una
pista no puede impedir que suenen las demás.

### Cómo se garantiza "sin asignar ni bloquear"

- **Sin bloqueos, en compilación.** `crates/voltra-audio/clippy.toml` con
  `disallowed-types` para `std::sync::{Mutex, RwLock, Condvar, Barrier}` y
  `disallowed-methods` para `std::thread::sleep`. Con `-D warnings`, un mutex
  en la crate rompe CI.
- **Sin asignaciones, en ejecución.** Un test de integración con un
  `#[global_allocator]` que cuenta asignaciones del hilo actual mientras un
  `thread_local!` está armado. Se arma alrededor de 10 000 llamadas a `mix()`
  con rampas, pistas que faltan, NaN y bloques mal alineados, y se exige cero.
- **Sin syscalls ni logging**: `mix()` no llama a `tracing`; los fallos se
  cuentan y el llamante decide cuándo informar fuera del hilo de audio.

## Pasos

1. Crate `voltra-audio`: manifiesto, `lib.rs` con `//!`, `Error`, `clippy.toml`
   con los tipos prohibidos. Alta en el workspace.
2. `Gain` y `Balance`, con validación y conversión a dB; las tres leyes de
   ganancia por canal como funciones puras.
3. `TrackParams`/`TrackControl`, `Mixer::new`, `add_track`, `remove_track`.
4. `mix()`: comprobaciones, aislamiento de no finitos, suma con y sin rampa,
   informe y contadores.
5. Tests de tiempo real: contador de asignaciones y control desde otro hilo.
6. `benches/mix.rs`, cifras en `docs/PERFORMANCE.md`, actualización de
   `ROADMAP.md`, `CLAUDE.md` §8 y este plan; puerta de calidad y commit.

Un solo commit `feat(audio): ...`, como el plan 011. Si el diff productivo pasa
de ~400 líneas, se parte en dos: tipos y leyes (pasos 1–2) y mezclador
(pasos 3–6).

## Verificación

**Suma y ganancia:**

- Una pista a unidad y centrada sale idéntica bit a bit.
- Dos pistas suman muestra a muestra; la suma puede pasar de 1 y no se recorta.
- `Gain::from_db(-6.0)` da 0,501; `db()` de vuelta en menos de 0,001 dB.
  `Gain::new` rechaza NaN, `inf`, negativos y más de `MAX`.

**Leyes:**

- Balance estéreo en 0: ganancia exactamente 1 en los dos canales.
- Balance en ±1: el canal contrario a 0 exacto, el propio a 1.
- **Continuidad:** barrer el balance de −1 a 1 en pasos de 0,001 nunca cambia
  una ganancia más de 0,01. Es el test que libobs no pasaría a la salida de su
  banda muerta.
- Paneo mono → estéreo: `L² + R² = g²` en todo el rango (potencia constante), y
  0,707·g en el centro.

**Rampas:**

- Cambiar la ganancia entre bloques produce una rampa lineal: primera muestra
  en la ganancia vieja, última en la nueva, monótona en medio.
- Silenciar no produce ningún salto entre muestras consecutivas mayor que el de
  la propia señal.
- Una pista nueva, o que vuelve tras faltar, entra con fundido desde 0.

**Aislamiento:**

- Una pista con un NaN no suma, cuenta un fallo, y las demás salen idénticas a
  la mezcla sin ella.
- Lo mismo con `inf`, con frecuencia distinta, con número de frames distinto,
  con `pts` desalineado, con un `TrackId` desconocido y con la misma pista dos
  veces en un bloque.
- `add_track` rechaza 5.1 en una mezcla estéreo y estéreo en una mono.

**Tiempo real:**

- Cero asignaciones en 10 000 bloques (test de asignador).
- Un hilo cambia ganancia, balance y silencio en bucle mientras otro mezcla
  10 000 bloques: ninguna muestra de salida excede la suma de entradas por
  `Gain::MAX`, y el test termina (nadie se bloquea).
- La crate no compila con un `Mutex` dentro (comprobado una vez a mano y
  documentado; el lint lo mantiene).

**Rendimiento**, por bloque de 1024 muestras a 48 kHz, en `docs/PERFORMANCE.md`:

| Caso | Por qué |
|---|---|
| 1, 8 y 64 pistas estéreo, ganancia estable | El coste base y cómo escala |
| 8 pistas estéreo, todas en rampa | El peor caso de un fader arrastrado |
| 8 pistas mono → estéreo | La otra ley |
| Comprobación de finitos sola | Lo que cuesta el aislamiento |

**Criterio de aceptación:** 8 pistas estéreo, en rampa, por debajo del **1 % del
bloque (213 µs)**. Es un umbral holgado a propósito —la estimación es de unos
pocos µs: 8 × 2048 multiplicaciones-suma sobre 64 KB que caben en L2—; si la
cifra real queda cerca del umbral, algo está mal en el bucle y eso es un
hallazgo, no un aprobado. Más los tests de tiempo real en verde.

## Riesgos

- **El bucle con rampa no vectoriza.** `g0 + Δ·ramp[k]` son tres flujos en
  `zip`; debería, pero no está garantizado. Plan B: rampa por tramos de 64
  muestras con ganancia constante en cada uno, que es un escalón de 1/16 del
  cambio, inaudible. Se decide con el benchmark, no antes.
- **`clippy.toml` por crate.** Clippy lo busca desde el manifiesto de la crate
  hacia arriba; si en la versión fijada no fuera así, la regla iría al
  `clippy.toml` raíz restringida con `#[allow]` en las crates que sí usan
  mutexes, o a un test que haga `grep`. Se comprueba en el paso 1.
- **El test de asignador mide menos de lo que parece.** Solo ve el hilo que lo
  arma, y `mix()` no lanza hilos, así que es suficiente; si algún día lo hiciera
  (rayon por canales), el test tendría que crecer. Anotado en el propio test.
- **La rampa de un bloque es demasiado corta o demasiado larga.** 21 ms es el
  orden de lo que usan las estaciones de audio (10–50 ms). Si en uso real se
  oye, es una constante; no condiciona el diseño.
- **Diseñar el control para una UI que no existe.** Mitigación: `TrackControl`
  es lo mínimo que necesita un fader —escribir, leer, ver fallos—, y todo lo
  estructural (añadir, quitar) queda en `&mut Mixer`, donde no hay que
  adivinar nada.

## Resultado

Puerta de calidad en verde: **223 tests** (desde 183), `clippy -D warnings`
limpio, `fmt` limpio. `voltra-audio` aporta 40: 33 unitarios, 3 de tiempo real
y 4 doctests.

### Lo que quedó hecho

La crate `voltra-audio` con `Gain`, `Balance`, las tres leyes de ganancia por
canal, `TrackControl` sobre atómicos, `Mixer` con `MixSpec`, `MixReport` y
`MixTotals`, el test de tiempo real `tests/realtime.rs` y `benches/mix.rs`.

### El criterio de salida de la fase, comprobado

- **Cero asignaciones** en 10 000 bloques que recorren todos los caminos:
  rampas, fundidos de silencio, paneo, NaN, desalineación, pistas repetidas y
  ausentes. Un test previo comprueba que el contador sí ve una asignación, para
  que el bueno no pase en vacío.
- **Ningún bloqueo posible**: se metió un `Mutex` a propósito en la crate y
  `clippy -D warnings` lo rechazó (`use of a disallowed type std::sync::Mutex`);
  después se retiró. El riesgo de que Clippy no leyera un `clippy.toml` por
  crate no se materializó.
- Un hilo martillea ganancia, balance y silencio mientras otro mezcla 10 000
  bloques: ninguna muestra sale no finita ni por encima del techo teórico.

### Las cifras

En `docs/PERFORMANCE.md` §2. **8 pistas estéreo, todas en rampa: 3,24 µs**, el
0,015 % del bloque de 21,3 ms y **65× por debajo** del umbral de aceptación
(213 µs). La estimación del plan —"unos pocos µs"— se cumplió. La escala es
lineal (64 pistas, 20,9 µs), la rampa cuesta un 20 % sobre la ganancia estable,
así que el plan B de rampa por tramos no hizo falta, y el aislamiento de NaN es
hasta la mitad del coste, que en absoluto sigue siendo despreciable.

## Desviaciones respecto al plan

- **Dos commits en vez de uno**, como preveía el propio plan: el código
  productivo, sin comentarios ni tests, salió en ~500 líneas. El primero lleva
  la crate, los tipos y las leyes; el segundo, el mezclador. El primero marca
  `Routing` con `allow(dead_code)` porque su consumidor llega en el segundo, que
  lo quita.
- **`add_track` devuelve solo `TrackControl`**, no `(TrackId, TrackControl)`: el
  control ya sabe su `id()`, y la tupla repetía el dato.
- **`mix` devuelve `Result<MixReport>`.** El plan decía que de `mix()` no sale
  ningún error, y sigue siendo cierto para las pistas: ningún fallo de pista se
  propaga. Pero un búfer de salida con otro formato es un error del llamante,
  no de una pista, y silenciarlo sería peor. `Error::OutputMismatch` solo lleva
  datos `Copy`, así que construirlo no asigna, y `out` queda intacto.
- **La rampa es `(k + 1) / frames`**, no `k / (frames − 1)`: la primera muestra
  ya está un paso más allá de la ganancia vieja y la última cae en la nueva. El
  bloque anterior terminó en la vieja, y repetirla sería retener una muestra. El
  test comprueba esta forma en lugar de "primera muestra en la ganancia vieja".
- **Errores:** `EmptyBlock` en lugar de `InvalidSpec`, porque es el único caso;
  y `TooManyTracks`, no previsto, porque los identificadores no se reutilizan y
  `u32` se puede agotar en teoría.
- **Repetidas y desconocidas no cuentan como fallo de pista**, solo como
  `rejected`: la primera copia de una pista repetida sí suena, y una pista
  desconocida no tiene a quién apuntarle el fallo. Un bloque que no llega
  tampoco es un fallo: va a `missing`.
- **Dos lints de Clippy permitidos con razón escrita**: `eq_op` en
  `all_finite`, donde `x - x` es la prueba misma, y `float_cmp` en la
  comparación de ganancias, donde la igualdad exacta es lo que se quiere decir
  ("no cambió desde el bloque anterior"). En los tests se permite `float_cmp`
  porque comprueban resultados exactos a propósito.
