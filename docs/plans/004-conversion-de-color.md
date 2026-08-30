# 004 — Conversión de color RGB → YUV

- **Fase:** 1
- **Estado:** completado con desviación (2026-08-30) — ver Resultado

## Objetivo

`voltra-core` gana la conversión del lienzo compuesto (BGRA/RGBA) al formato que
comen los encoders (I420 y NV12), en BT.601 y BT.709, rango limitado y completo.
Aritmética entera de punto fijo, bucles escritos para vectorizar, y benchmark
con el coste real por frame a 1080p.

## Fuera de alcance

- **La dirección inversa** (YUV → RGB), que hace falta para cámaras y ficheros.
  No hay consumidor hasta la fase 5; se hará cuando lo haya, con este mismo
  aparato de coeficientes ya montado.
- Escalado. Convertir y reescalar son cosas distintas; mezclarlas es cómo se
  acaba con una función que hace las dos cosas mal.
- 10 bits, HDR y BT.2100. El aparato queda preparado (los coeficientes se
  derivan de Kr/Kb), pero sin consumidor no se escribe.
- Pool de frames: plan 005.

## Referencia

De `libobs/media-io/video-matrices.c`, verificado en el código:

| Espacio | Kb | Kr |
|---|---|---|
| `VIDEO_CS_601` | 0,114 | 0,299 |
| `VIDEO_CS_709` | 0,0722 | 0,2126 |
| `VIDEO_CS_2100_PQ` | 0,0593 | 0,2627 |

Rango parcial (limitado): luma 16–235, croma 16–240, negro `[16, 128, 128]`.
Rango completo: 0–255, negro `[0, 128, 128]`. OBS deriva todas sus matrices de
Kr/Kb con `initialize_matrix()` en vez de tabularlas a mano. Su configuración por
defecto —y la recomendada— es **NV12 + Rec.709 + limitado**, que será también la
nuestra.

**Qué adoptamos:** los coeficientes, los rangos, la derivación desde Kr/Kb, y el
valor por defecto.

**Qué mejoramos:**

1. OBS convierte en GPU por el camino principal y delega el camino CPU en
   `swscale` (FFmpeg), una dependencia nativa enorme para una operación que cabe
   en doscientas líneas. Aquí el camino CPU es propio y sin dependencias, así que
   sigue funcionando en el contenedor headless y en CI.
2. Espacio y rango viajan **en el tipo**, no en variables sueltas. El fallo
   clásico —convertir en 601 y decodificar en 709, o marcar limitado un contenido
   en rango completo— produce un desplazamiento de color sutil que nadie nota
   hasta que el stream está publicado. Aquí no se puede convertir sin declarar
   ambos.
3. Cero números mágicos: los coeficientes de punto fijo se **derivan** de Kr/Kb y
   del rango en tiempo de compilación, y hay un test que compara cada uno con la
   fórmula en coma flotante.

## Diseño

### `color.rs` — el aparato de coeficientes

```rust
pub enum ColorSpace { Bt601, Bt709 }     // #[non_exhaustive]
pub enum ColorRange { Limited, Full }    // #[non_exhaustive]
pub struct ColorSpec { space, range }    // Default: Bt709 + Limited, como OBS
```

De ahí se derivan, en `const fn`, los nueve coeficientes de punto fijo Q16 más
los dos desplazamientos:

```
Y  = ((yr·R + yg·G + yb·B + ½) >> 16) + y_offset
Cb = ((ur·R + ug·G + ub·B + ½) >> 16) + 128
Cr = ((vr·R + vg·G + vb·B + ½) >> 16) + 128
```

Q16 sobra: el producto mayor es ~0,72 × 255 × 65536 ≈ 1,2·10⁷, muy dentro de
`i32`, y el error de redondeo queda por debajo de medio LSB de 8 bits.

### `convert.rs` — los bucles

Dos funciones, `to_i420` y `to_nv12`, que comparten el mismo núcleo por bloque
2×2. Reglas de escritura del bucle, tomadas del hallazgo del plan 003 (una
lectura mal formada va 5× más lenta que la memoria):

- Recorrido por **pares de filas**, que es la unidad natural de 4:2:0: cada
  bloque 2×2 produce cuatro lumas y una pareja de cromas.
- `chunks_exact` en todo: origen, luma y croma. **Ningún `[i]`**, para que no
  haya comprobación de límites dentro del bucle.
- Croma promediando los cuatro píxeles RGB del bloque **antes** de convertir, no
  después: una suma de cuatro en vez de cuatro conversiones.
- Aritmética entera. Nada de `f32` por píxel.

Las dimensiones pares ya están garantizadas por el constructor de `VideoFrame`
(plan 003), así que los bucles no necesitan caso especial para el borde: esa
invariante se paga una vez y se cobra en cada frame.

`VideoFrame` gana `planes_mut()`, que entrega los tres planos mutables a la vez
mediante `split_at_mut` — sin él habría que recorrer el origen dos veces, una
para luma y otra para croma, y a 1080p60 esa segunda pasada cuesta milisegundos.

## Pasos

1. `color.rs`: espacios, rangos y derivación de coeficientes, con el test que
   los contrasta contra la fórmula en coma flotante.
2. `frame.rs`: `planes_mut()`.
3. `convert.rs`: `to_i420` y `to_nv12`, con tests de valores conocidos.
4. `benches/convert.rs`: coste por frame a 1080p.
5. Puerta de calidad y resultados anotados aquí.

## Verificación

- Cada coeficiente entero coincide con la fórmula en `f64` dentro de ±1 LSB.
- Colores conocidos, verificados a mano contra el estándar: negro, blanco, rojo,
  verde, azul y gris medio, en 601 y 709, limitado y completo. En limitado, el
  negro debe dar exactamente `Y=16` y el blanco `Y=235`; en completo, `0` y
  `255`.
- Un gris neutro produce croma exactamente `128` (si no, hay un sesgo de color).
- Formatos incompatibles devuelven `Error::Unsupported`, no un pánico.
- Tamaño distinto entre origen y destino: error, no corrupción.
- El plano de croma de NV12 alterna U y V en el orden correcto.

Criterio de aceptación: **BGRA → NV12 a 1080p por debajo de 3 ms**. Es holgado a
propósito: el número real que buscamos es la línea base para cuando llegue el
camino GPU, que debería dejarlo cerca de cero.

## Resultado

Puerta de calidad en verde: 63 tests (60 unitarios, 1 en `voltra-cli`, 2
doctests). Funciona BGRA/RGBA → I420/NV12 en 601 y 709, limitado y completo.

| Caso | Mediana | Rendimiento |
|---|---|---|
| BGRA → NV12, 720p | 1,87 ms | ~495 Mpx/s |
| BGRA → I420, 720p | 1,86 ms | ~494 Mpx/s |
| BGRA → NV12, 1080p | **4,11 ms** | ~482 Mpx/s |
| BGRA → I420, 1080p | **4,18 ms** | ~478 Mpx/s |

### El criterio de aceptación NO se cumple

Puse < 3 ms a 1080p y el resultado es 4,1 ms: el **25 % del presupuesto de
frame** para una sola operación. Se registra como incumplido en vez de moverse
la portería.

Dos experimentos, uno por hipótesis:

1. **Separar el bucle en dos pasadas** (una de luma elemento a elemento y otra
   de croma) para que el compilador vectorizara la de luma: **sin efecto**,
   ±1 %, dentro del ruido. Se revirtió; no se paga una lectura extra del origen
   a cambio de nada.
2. **`clamp` sin ramas** (`value.clamp(0, 255)` en vez de `if/else if/else`):
   **−11 %**, de 4,78 ms a 4,30 ms, y de forma consistente en los cuatro casos.
   Se queda. El precio fue dejar de ser `const fn`, que no se estaba usando.

Con 482 Mpx/s se mueven ~2,6 GB/s, muy lejos de los 20 GB/s que la propia
máquina alcanza escribiendo (plan 003). Sigue siendo **límite de cómputo, no de
memoria**: son unas 4,5 multiplicaciones por píxel en código escalar y el
compilador no está bajando a SIMD.

### Qué lo arreglaría, y por qué no se hace aquí

- **SIMD explícito** (AVX2 con detección en tiempo de ejecución). Es lo que hace
  `libyuv` y ahí está el 4×. Requiere `unsafe`, y CLAUDE.md §3 exige para eso
  justificación escrita, aislamiento y tests propios: es un plan aparte, no una
  línea colada en este.
- **Paralelizar por filas con `rayon`.** Con 4 núcleos dejaría esto en ~1,2 ms.
  No se hace ahora a propósito: el modelo de hilos (CLAUDE.md §5) todavía no
  existe, y repartir la conversión por todos los núcleos antes de saber quién
  más los necesita —el encoder, sobre todo— es optimizar a ciegas.
- **La GPU**, que es el destino real según el ADR 0001: allí esta conversión es
  prácticamente gratis y ya está pagada por el compositor. El camino CPU es la
  referencia verificable y el respaldo, no el camino principal.

Conclusión: 4,1 ms es aceptable **como respaldo**, y queda como línea base con la
que comparar. No es aceptable como camino principal, y no se pretendía que lo
fuera.

## Desviaciones respecto al plan

- El criterio de < 3 ms no se cumple (arriba, con el detalle).
- `luma`, `chroma` y `clamp_u8` dejaron de ser `const fn` a cambio del 11 %.
  Nadie las llamaba en contexto `const`; la derivación de coeficientes, que sí se
  usa así, sigue siendo `const` y tiene test que lo comprueba.
- Se añadió `VideoFrame::planes_mut()`, previsto en el diseño: entrega los tres
  planos mutables a la vez con `split_at_mut`, sin `unsafe`.

## Riesgos

- **El promediado del croma admite varias definiciones.** Promediar RGB y luego
  convertir no da exactamente lo mismo que convertir y luego promediar. Se elige
  la primera (más barata, y es lo que hace `libyuv`) y se documenta, para que
  quien compare contra FFmpeg sepa por qué difiere en ±1.
- **Sin dato de vectorización real.** Si el bucle no baja a SIMD, el número lo
  dirá y habrá que mirar el ensamblador. No se optimizará a ciegas.
