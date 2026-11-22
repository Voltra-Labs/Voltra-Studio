# 014 — Remuestreo

- **Fase:** 3
- **Estado:** en curso (2026-11-22) — aprobado sin revisión previa por
  indicación expresa del responsable ("no me preguntes, dale")

## Objetivo

Un `Resampler` en `voltra-audio` que convierte un flujo de bloques de una
frecuencia a otra —44,1 ↔ 48 kHz y las demás estándar— con un filtro polifase
de razón exacta, banda pasante plana hasta 20 kHz y aliasing por debajo de
−100 dB, **sin asignar memoria al procesar**, con la salida alineada en el
tiempo con la entrada y marcas de tiempo exactas. Es lo que permite que una
fuente de 44,1 kHz entre en una mezcla de 48 kHz.

## Fuera de alcance

- **Reagrupar en bloques fijos.** El remuestreador entrega lo que salga —1114 o
  1115 muestras por cada 1024 a 44,1 → 48 kHz— y el mezclador quiere bloques de
  1024 exactos. La cola que los reagrupa es el buffering de la sincronía A/V, el
  plan siguiente; aquí no se conecta el remuestreador al mezclador.
- **Razón variable** para corregir la deriva entre relojes de dispositivos
  (±0,01 %): también es de la sincronía.
- **Razones que no caben en 1024 fases.** Entre las frecuencias estándar
  (8 000 a 192 000 Hz) no hay ninguna; se rechazan con error.
- **Vaciar el filtro al final** (`flush`): las últimas ~64 muestras de entrada
  de una grabación que termina. Es 1,4 ms y una emisión no termina así; se
  añade cuando exista la grabación a fichero.
- **Remezcla de disposiciones**: el remuestreador mantiene la de la entrada.
- **La ganancia de mono → estéreo** de OBS (0 dB por lado frente a los −3 dB
  del plan 012): hallazgo de la referencia, decisión del mezclador, no de este
  plan.

## Referencia

`docs/references/audio-resampling.md`, escrito para este plan a partir del
envoltorio de OBS y de `libswresample`.

**Qué adoptamos:** polifase de razón racional exacta, sinc con ventana de
Kaiser, y la salida alineada en el tiempo con la entrada.

**Qué mejoramos:**

1. **Filtro de 128 coeficientes, β = 12, con corte al 0,95 del Nyquist menor
   también al subir** (§3, §4.1). swr corta en el Nyquist exacto al subir y
   deja una imagen a −25 dB con un tono de 20 kHz; bajando, repliega un tono de
   23 kHz a −20 dB. Con este filtro, los mismos casos dan −133 dB y −121 dB.
   Cuesta 4× las multiplicaciones, y se mide.
2. **Cada fase normalizada a ganancia 1 en continua** (§2), en vez de todas por
   la suma de la fase 0.
3. **Sin asignaciones** (§1): swr reasigna su búfer de salida cuando un bloque
   crece; aquí todo se reserva al crear.
4. **Alineación exacta por construcción** (§4.4): la muestra *k* de salida es la
   entrada interpolada en el instante *k* / *f*<sub>salida</sub>, sin restar un
   retardo a la marca de tiempo.

## Diseño

### El banco de filtros

```rust
struct FilterBank {
    up: usize,         // L: output rate / gcd
    down: usize,       // M: input rate / gcd
    taps: usize,       // per phase, multiple of 8
    center: usize,     // (taps - 1) / 2
    coefficients: Vec<f32>,   // up × taps, phase-major
}
```

- `L/M` es la razón reducida: 44,1 → 48 kHz es 160/147.
- `factor = min(1, f_out / f_in) × 0,95`: el corte relativo al Nyquist de la
  entrada.
- `taps = ⌈128 / factor⌉` redondeado a múltiplo de 8, para que el producto
  escalar vaya en carriles de 8 sin resto. Al subir, 136; al bajar de 48 a 44,1,
  152.
- Fase *p*, coeficiente *j*: `sinc(π·factor·t) · I₀(β·√(1 − (2t/taps)²))`, con
  `t = (j − center) − p/L`. Se calcula en `f64` al construir y se guarda en
  `f32`. Cada fase se divide por su propia suma.
- Rechaza `L > 1024` (`Error::UnsupportedRatio`) y la misma frecuencia en los
  dos lados (`Error::SameRate`: un remuestreador que no remuestrea es un error
  de quien lo crea).

### El remuestreador

```rust
pub struct Resampler { ... }

impl Resampler {
    pub fn new(from: SampleRate, to: SampleRate, layout: ChannelLayout,
               max_input_frames: usize) -> Result<Self>;
    pub fn max_output_frames(&self, input_frames: usize) -> usize;
    pub fn latency(&self) -> Duration;
    pub fn process(&mut self, input: &AudioBuffer, output: &mut AudioBuffer) -> Result<usize>;
    pub fn reset(&mut self);
    pub fn discontinuities(&self) -> u64;
}
```

**Posición exacta.** La salida *k* interpola la entrada en la posición
`k·M/L`. Se lleva en enteros: índice de la primera muestra de la ventana y fase
`0..L`; cada muestra de salida suma `M` a la fase y lleva el exceso de `L` al
índice. No hay coma flotante en la posición, así que no hay deriva.

**Ventana por canal.** Un búfer de trabajo por canal, reservado en `new` para
`taps + max_input_frames` muestras: lo que quedó del bloque anterior seguido
del bloque nuevo. Copiar el bloque a la ventana es la única copia —1024
muestras por canal—, y se documenta como inevitable: el producto escalar
necesita contigüidad a través del borde entre bloques.

**Arranque.** Al crear o reiniciar, la ventana empieza con `center` ceros: la
entrada antes del principio es silencio, y así la salida 0 cae exactamente en
la entrada 0. La primera salida espera a tener `taps − center` muestras por
delante: esa es la latencia, unos 1,5 ms.

**Marcas de tiempo.** La salida de cada bloque lleva
`inicio + to.pts(producidas)`, calculado desde el índice absoluto. El inicio es
la marca de tiempo del primer bloque de entrada.

**Continuidad.** Cada bloque de entrada debe empezar donde terminó el anterior
(`inicio + from.pts(consumidas)`), con media muestra de tolerancia para el
redondeo a nanosegundos. Si no, el flujo tiene un hueco o un salto: se
reinicia —ventana a ceros, contadores a cero, nuevo inicio— y se cuenta en
`discontinuities()`. Las marcas de tiempo con fluctuación de un dispositivo de
verdad no pasarán esta prueba: alisarlas antes es trabajo de la sincronía.

**Salida.** `process` escribe en un `AudioBuffer` del llamante, de frecuencia
`to` y la misma disposición, y le da el número exacto de muestras producidas con
`reshape`. Si no se produjo ninguna —un bloque diminuto al arrancar—, devuelve 0
y no toca la salida. Para que `reshape` nunca reasigne, la salida debe tener
capacidad para `max_output_frames(input.frames())`; se comprueba antes de
escribir nada.

Eso necesita un método nuevo en `voltra-core`:
`AudioBuffer::capacity_frames()`, las muestras por canal que caben sin
reasignar. Es una línea, tiene un consumidor nombrado y no cambia nada de lo que
existe.

**Errores**, todos con datos `Copy` para no asignar al construirlos:
`FormatMismatch` (frecuencia o disposición de la entrada distintas),
`BlockTooLarge` (más de `max_input_frames`), `OutputTooSmall`,
`UnsupportedRatio`, `SameRate`.

**El bucle.** Por canal, por cada salida: producto escalar de `taps` muestras de
la ventana por la fila de su fase, en carriles de 8 con `chunks_exact`. Se
recorre canal a canal (la ventana y la salida de un canal se quedan en caché) y
el paso de la posición se repite por canal: es aritmética entera, y da el mismo
estado final en todos.

## Pasos

Dos commits:

1. **`AudioBuffer::capacity_frames` y el banco de filtros**, con sus tests:
   razón reducida, número de coeficientes, ganancia 1 en continua por fase,
   razones rechazadas.
2. **`Resampler`**: ventana, posición exacta, marcas de tiempo, continuidad,
   errores; tests de exactitud, de partición y de tiempo real; benchmark;
   `PERFORMANCE.md`, `ROADMAP.md`, `CLAUDE.md` §8 y este plan.

## Verificación

**Calidad**, con la misma medida que la referencia —ajuste por mínimos
cuadrados—, en `f32`:

- 44,1 → 48 y 48 → 44,1 kHz, tonos de 1, 10, 18 y 20 kHz: ganancia dentro de
  **±0,05 dB** y error por debajo de **−110 dB**. El prototipo en `f64` da como
  mucho −0,017 dB y −126 dB; el margen es para el redondeo de `f32`.
- 48 → 44,1 kHz, tono de 23 kHz: la salida entera por debajo de **−100 dB**
  (prototipo: −121 dB).

**Corrección del flujo:**

- **Partir en bloques no cambia nada**: el mismo flujo en bloques de 1024 o en
  bloques de 1, 7, 100 y 1500 da las mismas muestras, bit a bit, y el mismo
  total.
- **Sin deriva**: tras 10 s de entrada, las muestras producidas difieren de
  `entrada × L/M` en menos de la latencia, y las marcas de tiempo de bloques
  consecutivos encajan exactamente.
- **Alineación**: un tono a la entrada y el mismo tono generado directamente a la
  frecuencia de salida coinciden en fase (error por debajo de −100 dB sin
  ajustar la fase).
- Un salto en la marca de tiempo de entrada reinicia, cuenta una
  discontinuidad y la salida siguiente empieza en la nueva marca.
- Entrada de otra frecuencia o disposición, bloque mayor que el máximo y salida
  sin capacidad dan su error sin tocar nada.
- Un bloque diminuto al arrancar devuelve 0 y deja la salida intacta.
- **Cero asignaciones** en `process` durante miles de bloques.

**Rendimiento**, por bloque de 1024 muestras estéreo de entrada:

| Caso | Criterio |
|---|---|
| 44,1 → 48 kHz | **< 1 % del bloque (213 µs)**; estimación 30–60 µs |
| 48 → 44,1 kHz | Anotar |

## Riesgos

- **El producto escalar no vectoriza.** Mismo patrón que el pico verdadero, que
  sí lo hizo; si no, carriles explícitos de 16, que es lo que funcionó en el
  plan 013.
- **El coste de 4× swr se nota con muchas fuentes.** Ocho fuentes a 44,1 kHz a
  50 µs son el 2 % del bloque. Si un día importa, la palanca es un filtro más
  corto como opción de calidad, con su medida de lo que pierde; no se ofrece
  antes de que alguien lo necesite.
- **La tabla de fases crece con razones raras**: 11 025 → 48 000 son 640 fases ×
  136 coeficientes, 348 KB. Cabe en L2 de casi cualquier CPU; si no, se mide.
- **La tolerancia de continuidad es demasiado estricta para fuentes reales.** Es
  deliberado: este remuestreador asume un flujo continuo y la sincronía le
  entregará uno. Si se quiere usar antes con un dispositivo, `reset` existe.
