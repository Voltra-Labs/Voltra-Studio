# 003 — Formatos de píxel y `VideoFrame`

- **Fase:** 1
- **Estado:** completado (2026-08-30)

## Objetivo

`voltra-core` gana el tipo que transporta imagen: `PixelFormat` con la geometría
de planos de cada formato, y `VideoFrame`, un frame de CPU con **una sola
asignación**, planos contiguos, *stride* explícito y alineación apta para SIMD.
Con benchmarks de asignación y de recorrido por filas como línea base.

## Fuera de alcance

- **Conversión de color** (BGRA↔I420/NV12). Es el primer bucle por píxel de
  verdad y merece su propio paso, con imágenes doradas y benchmark: plan 004.
- **Pool de frames.** Es el mecanismo que hace cumplir "cero asignaciones por
  frame" (CLAUDE.md §4.1), pero necesita el tipo `VideoFrame` ya cerrado: plan
  005.
- Texturas de GPU. Ver la nota de diseño más abajo: no van en este tipo.
- Formatos de 10/12/16 bits, `I444`, `YUY2` y compañía. Se añadirán cuando haya
  un consumidor que los pida.

## Referencia

Del código de libobs (`libobs/media-io/video-io.h` y `video-frame.c`):

- `struct video_data { uint8_t *data[MAX_AV_PLANES]; uint32_t linesize[MAX_AV_PLANES]; uint64_t timestamp; }`.
- `video_frame_init` hace **una sola asignación** para todos los planos: calcula
  `linesize × height` por plano, lo **alinea hacia arriba** con
  `base_get_alignment()`, y guarda los offsets acumulados
  (`frame->data[i] = frame->data[0] + offsets[i - 1]`).
- Los `linesize` por formato: I420 → `width`, `width/2`, `width/2`; NV12 →
  `width`, `width` (croma entrelazada); BGRA → `width * 4`; I444 → `width` ×3.

**Qué copiamos:** exactamente ese modelo. Una asignación, planos contiguos con
offsets alineados, *stride* independiente del ancho. Es lo correcto para la caché
y lo que las APIs de captura y encoding esperan recibir.

**Qué mejoramos:**

1. En C, `data[i]` y `linesize[i]` son arrays paralelos que nadie obliga a
   mantener coherentes, y `format` no impide leer un plano que no existe. Aquí
   la geometría de planos **se deriva del formato**, no se guarda a mano: es
   imposible construir un frame cuyos strides no correspondan a su formato.
2. Las dimensiones impares con croma 4:2:0 son un error silencioso clásico
   (media fila de croma que nadie escribe). Aquí se rechazan en el constructor.
3. `rows()` entrega filas **sin el relleno del stride**, de largo exactamente
   `ancho × bytes`, para que los bucles que las consuman sean `chunks_exact` y el
   compilador pueda autovectorizarlos.

### Nota de diseño: esto no cierra el camino sin copia

Preocupación legítima tras el ADR 0003: si el frame se define como bytes en CPU,
el camino GPU→encoder sin copia queda muerto.

No ocurre, y la razón es que **libobs tampoco mete las dos cosas en el mismo
tipo**: `obs_source_frame` es el frame de CPU (fuentes asíncronas: cámaras,
ficheros, capturas que entregan memoria), y `gs_texture_t` es la textura de GPU
del camino de render. Son tipos distintos que conviven.

Copiamos esa separación. `VideoFrame` es el frame **de CPU**. La textura vivirá
en `voltra-render` (no puede estar en `voltra-core`: depende del backend
gráfico). Quien tiene que aceptar ambas es la **entrada del encoder**, y eso se
decide en el plan del encoder, no aquí. Lo que este paso sí garantiza es que
nada del pipeline se escribe contra `&[u8]` suelto.

## Diseño

### `PixelFormat`

Cinco formatos, cada uno con un consumidor real hoy o en el paso siguiente:

| Formato | Planos | Para qué |
|---|---|---|
| `Bgra8` | 1 | Lienzo de composición. Es el orden que usa el camino de render de OBS. |
| `Rgba8` | 1 | Interoperabilidad: ficheros de imagen, `egui`, tests. |
| `Nv12` | 2 | Lo que quieren los encoders hardware. |
| `I420` | 3 | Lo que aceptan todos los encoders software. |
| `Y8` | 1 | Luma sola: máscaras, luma key, tests baratos. |

`#[non_exhaustive]`: crecerá con 10 bits y 4:4:4.

La geometría se deriva del formato, no se almacena:

```rust
fn plane_count(self) -> usize;
fn plane_stride(self, plane: usize, width: u32) -> Option<usize>;
fn plane_height(self, plane: usize, height: u32) -> Option<u32>;
fn requires_even_size(self) -> bool;   // cierto para 4:2:0
fn has_alpha(self) -> bool;
```

### `VideoFrame`

```rust
pub struct VideoFrame {
    format: PixelFormat,
    size: FrameSize,
    pts: Timestamp,
    planes: [Plane; MAX_PLANES],  // en línea, sin asignación aparte
    plane_count: usize,
    data: Vec<u8>,                // una sola asignación, planos contiguos
}
```

`Plane { offset, stride, width, height }`. `MAX_PLANES = 3`, suficiente para todo
lo declarado; ampliarlo es cambiar una constante.

Alineación de plano: **32 bytes**, el ancho de un registro AVX2. Es la misma
decisión que `base_get_alignment()` en libobs. Cada offset de plano se redondea
hacia arriba, de modo que todo plano empieza alineado y un bucle SIMD no necesita
prólogo.

## Pasos

1. `pixel.rs`: `PixelFormat` y su geometría de planos, con tests por formato.
2. `frame.rs`: `FrameSize`, `Plane`, `VideoFrame` y sus accesos por filas.
3. `benches/frame.rs`: asignación 1080p y recorrido por filas.
4. Reexportar en `lib.rs`.
5. Puerta de calidad y resultados anotados aquí.

## Verificación

- Geometría por formato a 1920×1080: I420 → strides 1920/960/960 y alturas
  1080/540/540; NV12 → 1920/1920 y 1080/540; BGRA → 7680 y 1080.
- Todo offset de plano es múltiplo de 32.
- Dimensiones impares: rechazadas en I420 y NV12, aceptadas en BGRA e Y8.
- Dimensión cero: rechazada siempre.
- `rows()` entrega exactamente `height` filas de `width × bytes_por_píxel`.
- Pedir un plano que no existe devuelve `None`, no un pánico ni basura.
- El tamaño total del búfer coincide con la suma de planos alineados.

Criterio de aceptación: asignar un frame 1080p BGRA por debajo de **1 ms** (es
memoria; el número real interesa como línea base para cuando llegue el pool, que
debe dejarlo en cero).

## Resultado

Puerta de calidad en verde. 45 tests (43 unitarios en `voltra-core`, 1 en
`voltra-cli`, 1 doctest).

Benchmarks a 1080p (`cargo bench -p voltra-core --bench frame`):

| Caso | Mediana | Lectura |
|---|---|---|
| Asignar frame BGRA (8,29 MB) | **387 µs** | 2,3 % del presupuesto de frame |
| Asignar frame I420 (3,11 MB) | **133 µs** | ~43 µs/MB, o sea: es el puesta a cero |
| Asignar frame NV12 | **143 µs** | — |
| Leer todas las filas BGRA | **1,99 ms** | 4,2 GB/s |
| Escribir todas las filas BGRA | **410 µs** | 20,2 GB/s |

El criterio de aceptación (< 1 ms por asignación) se cumple, pero el número
importante es otro: **asignar cuesta ~46 µs por megabyte y escala con el
tamaño**, o sea que es el `vec![0; n]` tocando páginas. A 1080p60 son 23 ms por
segundo de puro trabajo inútil. Justifica el pool del plan 005 con un número, no
con una intuición.

### Hallazgo: la forma del bucle de lectura importa cinco veces

Leer 8,29 MB cuesta 1,99 ms (4,2 GB/s) mientras que escribirlos cuesta 410 µs
(20,2 GB/s). La lectura no está limitada por la memoria: está limitada por la
forma del bucle. `row.iter().copied().map(u64::from).sum()` acumula byte a byte
en un único registro y crea una cadena de dependencias que impide vectorizar; el
`fill()` de la escritura sí baja a instrucciones anchas.

No se toca ahora —no toca en este paso y no hay consumidor todavía—, pero queda
medido y anotado: **los bucles por píxel del plan 004 (conversión de color) y
del compositor deben escribirse para vectorizar**, y aquí está el número que
demuestra que la diferencia es de 5×, no cosmética.

## Desviaciones respecto al plan

- Ninguna en el diseño: `PixelFormat`, `VideoFrame` y la geometría derivada
  salieron como estaban planteados.
- `clippy::match_same_arms` obligó a fusionar ramas que se habían separado por
  legibilidad. La fusión quedó bien: el comentario explica por qué la fila de
  croma de NV12 mide lo mismo que la de luma, que era lo que las ramas separadas
  intentaban decir.
- El test de alineación usa 1918 px de ancho a propósito: es par (válido para
  4:2:0) pero no múltiplo de 32, así que ningún plano queda alineado por
  casualidad.

## Riesgos

- **Sobreespecificar formatos.** Añadir los 25 de libobs "por si acaso" sería
  código muerto que hay que mantener. Se añaden bajo demanda.
- **La alineación de 32 podría quedarse corta** si algún día se usa AVX-512 (64).
  Es una constante única y documentada; cambiarla no rompe la API.
- **`Vec<u8>` frente a asignación alineada.** `Vec` no garantiza que el inicio
  del búfer esté alineado a 32, solo los offsets **relativos** entre planos. Se
  asume en este paso y se resuelve en el plan del pool, que es quien controla la
  asignación real. Queda anotado para no darlo por hecho.
