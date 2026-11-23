# 015 — Entrada de audio: de la fuente al tick de la mezcla

- **Fase:** 3
- **Estado:** completado (2026-11-22) — ver Resultado. Ejecutado sin esperar
  revisión del plan, por indicación expresa del responsable ("no me preguntes,
  dale").

## Objetivo

Una cola de entrada por fuente, `AudioInput`, que recibe bloques del hilo de la
fuente —de cualquier tamaño, con la marca de tiempo que traigan, a la frecuencia
de la fuente— y entrega en el hilo de la mezcla exactamente el bloque de 1024
muestras que corresponde a cada tick, listo para `Mixer::mix`. Absorbe la
fluctuación de las marcas, rellena huecos, recorta solapes, descarta lo que
llega tarde, remuestrea si hace falta y lo cuenta todo. Sin mutex, sin
asignar y sin `unsafe`.

Con esto, una fuente de 44,1 kHz con marcas de tiempo fluctuantes suena en una
mezcla de 48 kHz sin un corte, que es lo que faltaba para que el camino del
audio esté completo.

## Fuera de alcance

- **Corrección de deriva entre relojes** (remuestreo de razón variable). Sin
  dispositivos no hay deriva que medir; vuelve con la captura, fase 5. Hasta
  entonces la deriva se ve en las métricas: el margen de la cola crece o mengua.
- **Atar el vídeo al reloj de audio.** Necesita el pipeline de grabación
  (fase 4), cuyo criterio de salida es precisamente la deriva A/V.
- **Relojes privados de fuente** (el `timing_adjust` de OBS). Todas las marcas
  se asumen en un mismo reloj monótono; quien tenga otro lo traduce antes.
- **`sync_offset` por fuente**, que llega con la interfaz que lo edita.
- **El hilo que conduce la mezcla**: elegir el tick y la latencia es del motor.
  Aquí se recibe el tick como argumento.

## Referencia

`docs/references/audio-sync.md`, escrito para este plan a partir de
`obs-source.c`, `obs-audio.c` y `obs.c`.

**Qué adoptamos:** alisar las marcas de tiempo por debajo de 70 ms; colocar por
marca de tiempo lo que no es contiguo; descartar lo que llega cuando su instante
ya se mezcló; poner tope a lo que acumula una fuente.

**Qué mejoramos:**

1. **Latencia explícita** (§3, §5.1): la fija quien conduce la mezcla; una
   fuente que no llega se queda sin bloque y las demás no esperan. OBS retrasa
   la mezcla entera y no vuelve a adelantarla nunca.
2. **Sin mutex ni reasignación** (§2): un anillo de un productor y un
   consumidor, reservado al crear. OBS reasigna su `deque` con el mutex que
   también toma la mezcla.
3. **Alisado antes del remuestreo** (§5.3), para que el remuestreador del plan
   014 vea un flujo exactamente continuo.
4. **Métricas por fuente** (§4, §5.4) y **el tope contado** (§2, §5.5).

## Diseño

Módulo nuevo `input` en `voltra-audio`.

### La línea de tiempo

Todas las posiciones son **índices absolutos de muestra a la frecuencia de la
mezcla**, desde el instante cero del reloj monótono: la muestra `i` está en
`mix_rate.pts(i)`. Una marca de tiempo `t` cae en la muestra
`round(t · rate / 10⁹)`, redondeando al más cercano para que `pts(i)` vuelva a
dar `i` pese al redondeo de nanosegundos de `pts`. Con índices absolutos no hay
origen que acordar entre productor y consumidor.

### El anillo

```rust
struct Ring {
    samples: Box<[AtomicU32]>,   // channels × capacity, f32 bits
    capacity: usize,             // power of two
    written: AtomicU64,          // absolute index one past the last written
    read: AtomicU64,             // absolute index one past the last consumed
}
```

- Muestra `i` del canal `c` en `c·capacity + (i mod capacity)`.
- **El productor** escribe muestras con `store(Relaxed)` y publica `written`
  con `Release`; **el consumidor** carga `written` con `Acquire`, lee con
  `Relaxed` y publica `read` con `Release`, que el productor carga con
  `Acquire` antes de sobrescribir. Es el SPSC clásico, y como cada muestra es
  un atómico no hay comportamiento indefinido posible, ni siquiera ante un uso
  incorrecto: en el peor caso, una muestra vieja.
- Un `store(Relaxed)` de `u32` es una instrucción de almacenamiento normal en
  x86 y ARM; el coste es no vectorizar la copia, y se mide.

### El productor: `InputWriter::push(&mut self, block)`

En el hilo de la fuente. Por bloque:

1. **Alisar** en el reloj de la fuente: si el bloque empieza a menos de
   `smoothing` (70 ms por defecto) de donde terminó el anterior, su marca pasa a
   ser exactamente esa; si no, es un hueco o un salto de verdad y la marca se
   respeta. La continuación esperada se calcula desde el índice absoluto, como
   en el remuestreador, nunca sumando duraciones.
2. **Remuestrear**, si la fuente no va a la frecuencia de la mezcla, con el
   `Resampler` del plan 014 y la marca ya alisada. Necesita
   `Resampler::process_at(input, pts, output)`, que es `process` con la marca
   explícita; `process` pasa a llamarla con la del bloque.
3. **Colocar** en el anillo la salida, en el índice de su marca de tiempo:
   - contiguo con lo anterior (±1 muestra de redondeo): se escribe a
     continuación;
   - más adelante: **hueco**, se rellena de silencio hasta su sitio;
   - más atrás: **solape**, se recorta lo que ya está escrito y se escribe el
     resto. Lo escrito no se reescribe: el consumidor podría estar leyéndolo;
   - lo que caiga antes de lo que el consumidor ya leyó es **tardío** y se
     recorta;
   - si no cabe sin pisar lo no leído, el bloque se **descarta** entero y se
     cuenta como desborde.

Nada de esto asigna: el remuestreador y su búfer de salida se reservan al crear.

### El consumidor: `InputReader::pull(&mut self, tick, out)`

En el hilo de la mezcla. `tick` es la marca de tiempo del bloque que se va a
mezclar; `out`, un bloque de la frecuencia y longitud de la mezcla en la
disposición de la fuente.

- Lo que haya en el anillo **antes** del tick se descarta: llegó cuando su
  instante ya se había mezclado. Se cuenta como tardío.
- Si están todas las muestras del tick: `Full`, y el bloque sale con la marca
  `tick`, directamente aceptable por `Mixer::mix` para la pista de esa fuente.
- Si no hay ninguna: `Empty`, y `out` no se toca. Quien mezcla omite la pista
  ese tick, y el mezclador la silencia con fundido y la cuenta como ausente
  (plan 012).
- Si hay parte: `Partial`, con el resto en silencio. Es un corte, y se cuenta.

### Métricas

`InputStats`, con atómicos que escriben los dos lados y cualquiera lee:
bloques entrados, huecos (veces y muestras), solapes y tardíos (muestras),
desbordes, discontinuidades del alisado, y por el lado del consumidor
`Full`/`Partial`/`Empty` y el **margen**: cuántas muestras había por delante del
final del tick en la última lectura, y el mínimo visto. El margen es la cifra
que dice si la latencia elegida basta, y la que mostrará la deriva cuando haya
dispositivos.

### Construcción

```rust
pub struct InputSpec {
    pub source_rate: SampleRate,
    pub layout: ChannelLayout,
    pub max_block_frames: usize,   // source side
    pub mix: MixSpec,
    pub capacity: Duration,        // ring length, rounded up to a power of two
    pub smoothing: Duration,       // 70 ms by default
}

pub fn audio_input(spec: InputSpec) -> Result<(InputWriter, InputReader)>;
```

La disposición es la de la fuente, no la de la mezcla: el anillo guarda mono si
la fuente es mono, y el mezclador hace el paneo (plan 012).

## Pasos

Tres commits:

1. **`Resampler::process_at` y el anillo SPSC**, con sus tests: orden de
   publicación, vuelta al principio del anillo, lectura concurrente.
2. **`InputWriter`, `InputReader` y `InputStats`**: alisado, remuestreo,
   colocación, tick; tests de cada regla.
3. **Extremo a extremo**: un tono a 44,1 kHz con marcas fluctuantes y bloques de
   tamaño variable, desde un hilo, a través de la entrada y del mezclador en
   otro, sale limpio a 48 kHz; benchmark; cero asignaciones en los dos lados;
   `PERFORMANCE.md`, `ROADMAP.md`, `CLAUDE.md` §8 y este plan.

## Verificación

**Reglas de colocación**, con bloques sintéticos:

- Bloques contiguos con marcas fluctuantes ±5 ms: se escriben seguidos, sin un
  hueco ni un solape, y el tick devuelve exactamente las muestras entradas.
- Un hueco de 30 ms por encima del umbral: el silencio ocupa su sitio, y la
  muestra que sigue cae en su índice exacto.
- Un bloque que se solapa con lo escrito: se recorta lo solapado, y lo escrito
  primero queda intacto.
- Un bloque cuyo instante ya se mezcló: se recorta como tardío.
- Un bloque que no cabe: se descarta y cuenta un desborde.
- Ticks antes de que llegue nada: `Empty`; con la mitad: `Partial`; con todo:
  `Full`, con la marca del tick.

**Extremo a extremo:**

- 10 s de un tono de 1 kHz a 44,1 kHz, en bloques de 441 a 1323 muestras con
  marcas fluctuantes ±5 ms, empujados desde un hilo; mezclados a 48 kHz en
  otro, con una latencia de 4 bloques. Tras el arranque, **ningún tick
  `Partial` ni `Empty`** y la salida coincide con el tono ideal a 48 kHz con un
  error por debajo de **−100 dB**.
- Ninguno de los dos hilos asigna.

**Rendimiento**, por bloque de 1024 estéreo a 48 kHz:

| Caso | Criterio |
|---|---|
| `push` sin remuestrear | < 0,1 % del bloque (21 µs) |
| `pull` | < 0,1 % del bloque |
| `push` con 44,1 → 48 kHz | el remuestreo (37 µs) + lo anterior |

## Riesgos

- **Las cargas y almacenamientos atómicos no vectorizan.** 2048 por bloque
  estéreo; aunque vayan de uno en uno, se estima 1–2 µs. Si pasara del umbral,
  la alternativa es un anillo de `UnsafeCell<f32>` con `unsafe` justificado
  —el SPSC es el ejemplo de manual—, y solo con la cifra delante.
- **El umbral de alisado oculta un hueco real de menos de 70 ms.** Es la
  elección de OBS y la razón es la misma: con dispositivos reales, la
  fluctuación es mucho más frecuente que un hueco de verdad. Es configurable.
- **`Partial` produce un corte seco.** Es un fallo de la fuente o de la latencia
  elegida, y se cuenta; suavizarlo con un fundido sería esconder la métrica.
- **Una fuente cuyo reloj va más rápido llena el anillo** hasta desbordar. Sin
  corrección de deriva es inevitable; las métricas lo enseñan antes de que
  pase, y la fase 5 lo corrige.

## Resultado

Puerta de calidad en verde: **295 tests** (desde 279), `clippy -D warnings`
limpio, `fmt` limpio. Tres commits: `Resampler::process_at` y el anillo; la
entrada con sus reglas; y la prueba extremo a extremo con el benchmark.

### Extremo a extremo

Diez segundos de un tono de 1 kHz a 44,1 kHz, en bloques de 441 a 1323
muestras con ±5 ms de fluctuación en las marcas, empujados desde un hilo y
mezclados a 48 kHz en otro con 85 ms de latencia:

- **Todos los ticks `Full`** tras los dos primeros (la latencia del
  remuestreador y el fundido de entrada de la pista).
- Ni huecos, ni solapes, ni discontinuidades; la fluctuación absorbida llegó a
  más de 4 ms.
- **Error de −136 dB** frente al tono ideal a 48 kHz, sin ajustar fase: el
  camino entero no añade nada al error del remuestreador.
- **Cero asignaciones en los dos hilos.**
- Margen mínimo antes de cada tick: 1976 muestras, 41 ms de los 85.

### Coste

`push` 0,95 µs, `pull` 0,99 µs por bloque estéreo de 1024; con remuestreo desde
44,1 kHz, `push` 38 µs, casi todo el remuestreo. Las atómicas no vectorizan,
como temía el plan, y da igual: veinte veces por debajo del umbral.

### Lo que los tests corrigieron

- **El arranque necesitaba una regla que el plan no tenía.** Las posiciones son
  índices absolutos del reloj monótono, así que el primer bloque cae en un
  índice enorme mientras la lectura está en cero: nunca cabría nada. Y rellenar
  de silencio desde cero habría servido ticks `Full` de silencio antes de que
  la fuente existiera. La regla: **todo índice entre la lectura y la escritura
  está escrito de verdad**; cuando no queda nada sin leer, el productor adelanta
  la lectura hasta su bloque en vez de rellenar. Para eso la posición de
  lectura avanza con `fetch_max` desde los dos lados, y el consumidor carga la
  escritura antes que la lectura (documentado en el anillo).
- **El alisado a cero convertía el redondeo en discontinuidades**:
  `pts(a) + pts(b)` difiere de `pts(a + b)` en un nanosegundo. El umbral nunca
  baja de media muestra de la fuente, como la tolerancia del remuestreador.
- **El primer bloque ancla la línea de tiempo.** Con fluctuación también en él,
  la salida entera quedaba desplazada hasta 5 ms y el error subía a −11,7 dB.
  Es el comportamiento correcto —el primer bloque dice dónde está la fuente, y
  OBS hace lo mismo—; el test fija la marca del primero y lo explica.

## Desviaciones respecto al plan

- **`push` devuelve `Result<()>`**, no un informe por bloque: todo lo que pasa
  se cuenta en `InputStats`, que leen los dos lados.
- **Un bloque más largo que el tick de salida** se detecta en `pull` con
  `OutputTooSmall` (el bloque debe medir exactamente un tick), y uno de otra
  disposición con `FormatMismatch`.
- **La capacidad mínima del anillo** es el doble del bloque más largo más un
  tick, aunque se pida menos, para que un bloque siempre quepa.
- **Fase 3 cerrada**, con la deriva de relojes movida a la fase 5 y el reloj A/V
  a la fase 4: decisión registrada en `ROADMAP.md`.
