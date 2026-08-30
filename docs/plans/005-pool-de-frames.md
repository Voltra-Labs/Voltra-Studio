# 005 — Pool de frames

- **Fase:** 1
- **Estado:** completado (2026-08-30)

## Objetivo

Eliminar la asignación por frame. `FramePool` recicla `VideoFrame` de una
geometría fija, de modo que en régimen estacionario un frame nuevo cuesta
prácticamente cero en vez de los 387 µs medidos en el plan 003.

## Fuera de alcance

- **Reciclado entre hilos.** El pool lo posee una etapa del pipeline y no lleva
  cerrojo. Devolver frames desde otro hilo será un canal, no un mutex, y se
  diseñará con el modelo de hilos (CLAUDE.md §5).
- Pools de audio o de texturas de GPU.
- Devolución automática por `Drop`. Requeriría `Arc<Mutex<…>>` o un canal, y un
  mutex en el camino de render está prohibido (CLAUDE.md §4.3). La devolución es
  explícita.

## Referencia

De `libobs/obs-source.c`, leído directamente:

```c
struct async_frame { struct obs_source_frame *frame; long unused_count; bool used; };
#define MAX_UNUSED_FRAME_DURATION 5
#define MAX_ASYNC_FRAMES 30
```

- `cache_video()` recorre `async_cache` buscando el primer hueco con
  `!used`; si lo encuentra lo marca usado y lo reutiliza, y si no, crea un frame
  nuevo y lo añade al array.
- `clean_cache()` incrementa `unused_count` de los que siguen libres y **destruye
  los que llevan 5 rondas sin usarse**, así que el pool se encoge cuando la
  demanda baja.
- `async_texture_changed()` compara formato, tamaño y rango; si algo cambió,
  `free_async_cache()` tira toda la caché.
- Si la cola pasa de `MAX_ASYNC_FRAMES`, se vacía entera y se descarta el frame:
  más vale perder frames de una fuente atascada que crecer sin límite.
- `remove_async_frame()` es la devolución: pone `used = false`.

**Qué copiamos:** la lista de libres, el envejecido de los inactivos a las 5
rondas, la purga al cambiar la geometría y el tope duro de ocupación.

**Qué mejoramos:**

1. En OBS la propiedad es una convención: el frame se marca `used` y se confía en
   que nadie lo toque, con un contador de referencias atómico por debajo. Aquí el
   pool **entrega el frame por valor**; mientras lo tienes, nadie más lo tiene.
   Usar un frame que sigue en la lista de libres no compila.
2. `cache_video` sostiene un mutex mientras recorre el array. Nuestro pool no
   lleva cerrojo porque no es compartido: es de la etapa que lo usa. Es la regla
   de CLAUDE.md §4.3 aplicada al diseño, no un detalle de implementación.
3. **El riesgo real del reciclado —píxeles rancios— se hace explícito.** Un frame
   reutilizado conserva la imagen anterior. `acquire()` lo dice en su
   documentación y existe `acquire_zeroed()` para quien no vaya a sobrescribir
   el frame entero.

## Diseño

```rust
pub struct FramePool { format, size, idle: Vec<IdleFrame>, capacity, stats }
struct IdleFrame { frame: VideoFrame, idle_rounds: u8 }
```

- `acquire(pts)` saca el último de la lista (LIFO: el más reciente es el que
  sigue caliente en caché) o asigna uno nuevo. Envejece los que quedan libres y
  suelta los que llegan a `MAX_IDLE_ROUNDS = 5`, como `clean_cache`.
- `release(frame)` valida geometría y lo devuelve a la lista si cabe; si no,
  lo suelta. Un frame de otra geometría se descarta y se cuenta, nunca se cuela.
- `reconfigure(format, size)` vacía la lista si algo cambió.
- `stats()` expone asignados, reutilizados, devueltos y descartados. CLAUDE.md
  §4.10: un pool sin contadores es un pool que nadie sabe si funciona.

## Pasos

1. `pool.rs`: `FramePool`, `PoolStats` y sus tests.
2. Reexportar en `lib.rs`.
3. `benches/pool.rs`: ciclo adquirir/devolver frente a `VideoFrame::new`.
4. Actualizar `docs/PERFORMANCE.md` con el resultado y cerrar la deuda §3.2.
5. Puerta de calidad.

## Verificación

- Un frame devuelto y readquirido es **el mismo búfer** (se compara el puntero).
- Con la lista vacía se asigna, y el contador de asignaciones lo refleja.
- Devolver un frame de otra geometría lo descarta y lo cuenta; nunca acaba en la
  lista de libres.
- La lista no supera nunca su capacidad.
- Un frame libre durante `MAX_IDLE_ROUNDS` adquisiciones se suelta.
- `reconfigure` a otra geometría vacía la lista.
- `acquire_zeroed` entrega ceros aunque el frame reciclado tuviera datos.
- `acquire` fija el *pts* pedido.

Criterio de aceptación: **el ciclo adquirir/devolver por debajo de 1 µs** a
1080p BGRA, frente a los 387 µs de asignar. Es un factor de 300; si no se
consigue, el pool no vale la pena y hay que entender por qué.

## Resultado

Puerta de calidad en verde: 74 tests (70 unitarios, 1 en `voltra-cli`, 3
doctests).

| Caso, 1080p BGRA | Mediana | Frente a asignar |
|---|---|---|
| `VideoFrame::new` (asignar) | 378 µs | — |
| `acquire` + `release` | **13,3 ns** | **28 400× más rápido** |
| `acquire_zeroed` + `release` | 344 µs | 1,1× |

El criterio de aceptación (< 1 µs) se cumple con setenta y cinco veces de
margen. En régimen estacionario el pool deja de asignar por completo: el test
`the_steady_state_stops_allocating` comprueba que cien adquisiciones consecutivas
producen **una** asignación.

### Hallazgo: `acquire_zeroed` casi no ahorra nada

Reciclar y poner a cero cuesta 344 µs frente a 378 µs de asignar de nuevo:
apenas un 9 %. La razón es que ambos caminos escriben los 8,29 MB completos, y
los dos lo hacen a unos 22–24 GB/s, que es el ancho de banda de escritura medido
en el plan 003.

O sea: **el ahorro del pool no viene de reutilizar la asignación, viene de no
escribir el búfer**. Quien necesite frames en blanco no obtiene prácticamente
nada del pool. La consecuencia de diseño es clara y queda documentada en la API:
las etapas del pipeline deben sobrescribir el frame entero, no pintar encima de
lo que había.

## Desviaciones respecto al plan

- Ninguna en el diseño. `FramePool`, el envejecido a las 5 rondas, la purga al
  reconfigurar y el tope de ocupación salieron como estaban planteados.
- Se añadió `PoolStats::hit_rate()`, no previsto: la proporción de aciertos es lo
  que se querrá enseñar en las estadísticas en vivo, y calcularla fuera obligaría
  a exponer los contadores en crudo a la UI.

## Riesgos

- **Píxeles rancios.** Es el precio del reciclado y la fuente más probable de un
  fallo visual raro. Mitigación: documentación explícita, `acquire_zeroed()`, y
  un test que comprueba que el reciclado efectivamente conserva los datos —para
  que el comportamiento sea una decisión y no una sorpresa.
- **Devolución explícita:** olvidar un `release()` no rompe nada, solo desperdicia
  el reciclado. Es una fuga de rendimiento, no de memoria; los contadores la
  harían visible.
