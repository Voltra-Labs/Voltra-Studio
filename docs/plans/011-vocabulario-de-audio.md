# 011 — Vocabulario de audio

- **Fase:** 3
- **Estado:** completado (2026-08-30) — ver Resultado

## Objetivo

El equivalente en audio de lo que el plan 003 hizo para vídeo: los tipos con los
que todo el resto de la fase 3 va a hablar. Frecuencia de muestreo, disposición
de canales, formatos de borde y `AudioBuffer` —float de 32 bits planar, una sola
asignación—, con conversión en los dos sentidos y aritmética de tiempo exacta.

## Fuera de alcance

Cada uno de estos es un plan propio de la fase 3, en este orden:

- **El mezclador.** Sumar pistas con volumen y paneo, sin asignar ni bloquear en
  el callback. Es el criterio de salida de la fase y necesita este paso primero.
- **Medidores** (pico, RMS, true peak).
- **Remuestreo.** 44,1 → 48 kHz es un filtro polifásico, con su propia medición.
- **Sincronía A/V.** Anclar el vídeo al reloj de audio.
- **Dispositivos.** PipeWire es la fase 5.
- **Pool de búferes.** El equivalente del plan 005, cuando haya un camino
  caliente que lo justifique medido.

## Referencia

`docs/references/audio.md`, escrito para este plan, más
`docs/references/obs-studio.md` §3.

**Qué adoptamos:** float de 32 bits planar como formato interno único, el
catálogo de `speaker_layout` para que una escena de OBS signifique lo mismo
aquí, y el principio de que el borde convierte y el núcleo no.

**Qué mejoramos:**

1. **El formato interno no es un valor de enum, es el tipo.** En libobs
   `audio_data` puede llevar cualquiera de nueve formatos y cada consumidor
   comprueba cuál. Aquí `AudioBuffer` **es** float planar por construcción: no
   hay campo que consultar ni caso que olvidar.
2. **Profundidad y disposición son dos ejes, no nueve valores.** Responder
   *¿es planar?* deja de ser enumerar media lista, y añadir una profundidad
   suma una entrada en vez de duplicarlas.
3. **El número de canales sale de la disposición**, en vez de ser un segundo
   campo que puede contradecirla.

## Diseño

Va en **`voltra-core`**, no en `voltra-audio`. `voltra-core` es el vocabulario
compartido y ya alberga `VideoFrame`; `voltra-audio` será el *procesado*
(mezclador, medidores, remuestreo). Si `AudioBuffer` viviera allí,
`voltra-capture` tendría que depender del mezclador solo para entregar muestras,
que es justo la dirección de dependencias que CLAUDE.md §2 prohíbe.

### `SampleRate`

`newtype` sobre `u32`, no un `u32` suelto (CLAUDE.md §3). Constantes para 44 100
y 48 000. Lo que aporta es la aritmética exacta:

```rust
pub fn pts(self, frame_index: u64) -> Timestamp;   // 128 bits, sin acumular
pub fn duration_of(self, frames: u64) -> Duration;
pub fn frames_in(self, duration: Duration) -> u64;
```

Es el mismo patrón que `Fps::pts` del plan 002 y por el mismo motivo: se calcula
desde el índice absoluto, nunca sumando duraciones. El audio manda en la
sincronía A/V (CLAUDE.md §5), así que su reloj es el que no puede derivar.

### `ChannelLayout`

Las siete disposiciones de `speaker_layout`. `channels()` deriva el número, que
no se almacena.

### Formatos de borde

```rust
pub enum SampleFormat { U8, I16, I32, F32 }   // profundidad
pub enum SampleOrder  { Interleaved, Planar } // disposición
pub struct SampleSpec { format, order }
```

Dos ejes en vez de los nueve valores de libobs. Solo se usan al convertir: nada
del núcleo los mira.

### `AudioBuffer`

```rust
pub struct AudioBuffer {
    rate: SampleRate,
    layout: ChannelLayout,
    frames: usize,       // muestras por canal
    pts: Timestamp,
    data: Vec<f32>,      // una asignación, canal c en [c*frames .. (c+1)*frames]
}
```

Una sola asignación con los canales contiguos, igual que `VideoFrame` hace con
los planos y por la misma razón: un búfer es un `malloc`, no ocho.

Sin `stride` ni relleno entre canales: a diferencia del vídeo, ninguna API de
audio entrega planos alineados a 32 bytes, y un plano de audio es un `&[f32]`
que ya está alineado a 4. Inventar relleno sería copiar una complicación del
vídeo que aquí no paga nada.

Acceso: `channel(i) -> Option<&[f32]>`, `channel_mut`, `channels_mut` para tocar
varios a la vez, `silence()` para poner a cero sin reasignar.

### Conversión

`fill_from(&[u8], SampleSpec)` y `write_to(&mut [u8], SampleSpec)`. Cada
combinación profundidad × disposición es un bucle monomórfico:
`chunks_exact` para no dejar comprobación de límites, y la profundidad como
parámetro genérico para que no haya `match` por muestra (CLAUDE.md §4.4). Es la
misma forma que el conversor de color del plan 004.

La escala entero↔float es la convención universal: `i16` divide por 32768 al
entrar y multiplica por 32767 con saturación al salir. Asimétrica a propósito —
`-32768` es representable y `+32768` no—, y documentada, porque es la clase de
detalle que produce un clic en el pico de cada onda.

## Pasos

1. `SampleRate`, `ChannelLayout` y su aritmética de tiempo.
2. `SampleFormat`, `SampleOrder`, `SampleSpec`.
3. `AudioBuffer`: asignación, acceso por canal, silencio.
4. Conversión en los dos sentidos.
5. `benches/audio.rs`.
6. Documentación, puerta de calidad y commit.

## Verificación

- **El tiempo no deriva.** `pts` de la muestra 48 000 a 48 kHz es exactamente
  1 s; a las 10 horas sigue siendo exacto. Se compara contra la suma acumulada
  de duraciones, que es lo que *no* se hace, para dejar la diferencia por
  escrito.
- **Ida y vuelta por cada formato.** `f32 → i16 → f32` vuelve dentro de un
  cuanto; `f32 → f32` vuelve idéntico bit a bit.
- **Intercalado y planar dicen lo mismo.** El mismo búfer escrito de las dos
  maneras y releído da los mismos canales.
- **Los extremos no envuelven.** `+1.0` y `-1.0` van a los extremos del rango
  entero, y `+2.0` satura en vez de dar la vuelta. Es el test que separa un
  limitador de un generador de ruido.
- **Cada disposición reporta sus canales**, y el búfer asigna exactamente
  `frames × canales` muestras.
- Un búfer de cero frames o de una disposición desconocida se rechaza al
  construir.
- Benchmark de conversión a 1024 muestras estéreo, la unidad de trabajo real.

Criterio de aceptación: los tipos existen, están documentados y el coste de
conversión de un bloque está medido y anotado en `docs/PERFORMANCE.md`. No hay
umbral que cumplir todavía: el presupuesto del callback lo fija el mezclador,
que es el plan siguiente.

## Riesgos

- **Diseñar para un mezclador que no existe.** Es el riesgo real de un paso de
  vocabulario. Mitigación: cada tipo aquí tiene un consumidor nombrado en el
  paso siguiente, y lo que no lo tiene —relleno entre canales, mezclas
  múltiples, formatos de 24 bits— se deja fuera explícitamente.
- **La conversión se convierte en el coste dominante.** A 48 kHz estéreo son
  96 000 muestras por segundo, tres órdenes de magnitud menos que los píxeles de
  1080p60. Se mide igualmente, porque "es poco" sin número es una opinión
  (§4.8).

## Resultado

Puerta de calidad en verde: **183 tests** (desde 155), `clippy -D warnings`
limpio, `fmt` limpio. `voltra-core` pasa de 87 a 112 tests propios.

### Lo que quedó hecho

`SampleRate`, `ChannelLayout`, `SampleFormat`, `SampleOrder`, `SampleSpec` y
`AudioBuffer`, en `crates/voltra-core/src/audio/`, más conversión en los dos
sentidos para las ocho combinaciones de profundidad × disposición y
`benches/audio.rs`.

### Las cifras

En `docs/PERFORMANCE.md` §2, por bloque de 1024 muestras estéreo. El peor caso
—codificar a `i16` intercalado— es **2,19 µs de los 21,3 ms** del bloque: el
0,01 %. Dos asimetrías quedaron anotadas sin actuar sobre ellas:

- **Codificar a `i16` cuesta 2,8× lo que decodificarlo**, por la sujeción al
  rango entero.
- **Intercalar cuesta 3–5× lo que copiar planos**, que es la transposición que
  la referencia anuncia como el precio del formato planar.

Ninguna se optimiza. Al 0,01 % del presupuesto, hacerlo sería exactamente la
optimización sin perfil que prohíbe CLAUDE.md §4.8.

### Un test que estaba mal y lo que enseñó

El test de deriva afirmaba que sumar duraciones redondeadas pierde *más de un
milisegundo* en media hora. **Es falso, y por dos órdenes de magnitud**: un
bloque de 1024 muestras a 48 kHz dura 21 333 333,33 ns, redondear pierde un
tercio de nanosegundo por bloque, y en media hora eso son **33 µs**. Muy por
debajo de cualquier error de sincronía audible.

El test corregido no afirma una magnitud, afirma la **forma**: la deriva crece
linealmente y no vuelve, mientras que calcular desde el índice absoluto cuesta
lo mismo y no deriva nunca. Ese es el argumento honesto para el diseño, y el
anterior era retórica con un número inventado encima.

## Desviaciones respecto al plan

- **El vocabulario vive en `voltra-core`, no en `voltra-audio`.** Estaba previsto
  y razonado en el plan, pero conviene repetirlo: `voltra-audio` se creará con el
  mezclador. Si `AudioBuffer` viviera allí, `voltra-capture` tendría que depender
  del mezclador solo para entregar muestras.
- **Se añadió `reshape`**, no previsto. Una fuente que cambia de disposición a
  mitad de sesión necesita rehacer su búfer, y hacerlo sin reasignar cuando el
  nuevo tamaño cabe es lo que mantiene el `malloc` fuera del hilo de audio
  (§4.3).
- **`I32` se soporta además de `U8`, `I16` y `F32`.** Sale casi gratis con el
  trait de profundidad y es lo que entregan las interfaces de audio decentes.
