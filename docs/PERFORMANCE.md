# Registro de rendimiento

Este documento es el estado medible del proyecto: qué se ha medido, cuánto
cuesta, y qué está pendiente de arreglar. **Ninguna entrada se escribe sin
número.** Se actualiza al cerrar cada plan que toque el camino caliente.

Reproducir todo:

```bash
cargo bench -p voltra-core
```

Máquina de referencia de las cifras actuales: x86_64, 4 hilos, contenedor Linux,
perfil `release` (`lto = "thin"`, `codegen-units = 1`).

---

## 1. Presupuesto

| Cadencia | Presupuesto por frame |
|---|---|
| 1080p60 | **16,6 ms** |
| 1080p30 | 33,3 ms |
| 4K60 | 16,6 ms (4× píxeles) |

Dentro de esos 16,6 ms tienen que caber captura, composición, filtros,
conversión, codificación y mux. El reparto objetivo da ~4 ms a la composición.

## 2. Mediciones actuales

| Operación | Coste | % de 16,6 ms | Plan |
|---|---|---|---|
| `Transform::place`, item suelto | 60 ns | 0,0004 % | 002 |
| Geometría de una escena de 50 items | 3,02 µs | 0,018 % | 002 |
| `Fps::pts` | 3,1 ns | — | 002 |
| Asignar frame BGRA 1080p | 387 µs | 2,3 % | 003 |
| Asignar frame I420 1080p | 133 µs | 0,8 % | 003 |
| Escribir todas las filas, BGRA 1080p | 410 µs | 20,2 GB/s | 003 |
| Leer todas las filas, BGRA 1080p | 1,99 ms | 4,2 GB/s | 003 |
| **BGRA → NV12, 1080p** | **4,11 ms** | **24,8 %** | 004 |
| BGRA → I420, 1080p | 4,18 ms | 25,2 % | 004 |
| BGRA → NV12, 720p | 1,87 ms | 11,3 % | 004 |

---

## 3. Deuda abierta

### 3.1 La conversión de color cuesta el 25 % del presupuesto

**Síntoma.** BGRA → NV12 a 1080p tarda 4,11 ms. El plan 004 fijó < 3 ms como
criterio de aceptación y no se cumple. Es la operación más cara del proyecto por
un margen enorme.

**Análisis.** El rendimiento es de ~482 Mpx/s, o sea ~2,6 GB/s de datos movidos.
La misma máquina escribe memoria a 20 GB/s (medición del plan 003), así que
**no está limitado por la memoria sino por el cómputo**: son unas 4,5
multiplicaciones enteras por píxel en código escalar que el compilador no está
bajando a instrucciones SIMD.

**Experimentos ya hechos** (para que nadie los repita):

| Hipótesis | Resultado |
|---|---|
| Separar el bucle en dos pasadas, luma y croma, para que vectorizara la de luma | **Sin efecto.** ±1 %, dentro del ruido. Revertido. |
| `clamp` sin ramas (`value.clamp(0, 255)`) en vez de `if/else if/else` | **−11 %**, de 4,78 a 4,30 ms. Aplicado. |

**Opciones de arreglo**, de mayor a menor ganancia esperada:

| Opción | Ganancia estimada | Coste | Riesgo |
|---|---|---|---|
| **Hacerlo en GPU** (ADR 0001) | Prácticamente gratis: la conversión la paga el compositor, que ya tiene el frame en textura | Requiere el backend `wgpu` funcionando | Bajo. Es el destino previsto, no un parche |
| **SIMD explícito** (AVX2 + NEON, con detección en tiempo de ejecución) | 3–4× → ~1,1 ms. Es la referencia de `libyuv` | ~300 líneas por arquitectura, `unsafe`, tests propios | Medio. CLAUDE.md §3 exige justificación, aislamiento y test por cada bloque `unsafe` |
| **Convertir *después* de reescalar** | 2,2× cuando se emite a menor resolución que el lienzo: 1080p→720p medido, 4,11 ms → 1,87 ms | Solo ordenar bien el pipeline | Ninguno. **Es lo que hace OBS**: `render_output_texture` (escalado) va antes que `render_convert_texture` (conversión) |
| **Paralelizar por filas con `rayon`** | ~3,5× con 4 núcleos → ~1,2 ms | Dependencia nueva y decisión de reparto de núcleos | Alto ahora mismo: el modelo de hilos (CLAUDE.md §5) todavía no existe, y quitarle núcleos al encoder sin saberlo es empeorar el total |
| **Saltarse la conversión** cuando el encoder acepte BGRA directamente | 4,11 ms → 0 | Depende del encoder; muchos hardware sí aceptan RGB | Ninguno, cuando aplica. Hay que consultarlo al encoder, no asumirlo |

**Decisión actual.** Se acepta 4,11 ms **como camino de respaldo**, que es
exactamente lo que el ADR 0001 dice que debe ser el camino CPU: correcto,
verificable y siempre disponible. No se acepta como camino principal. El orden
correcto de ataque es: (1) ordenar el pipeline para escalar antes de convertir,
que es gratis; (2) la GPU; y solo si tras eso la conversión en CPU sigue en el
camino caliente, (3) el SIMD explícito, con su propio plan y sus propias
mediciones.

### 3.2 Asignar un frame cuesta 387 µs

**Síntoma.** `VideoFrame::new` a 1080p BGRA tarda 387 µs, ~46 µs por megabyte, y
escala con el tamaño: es el `vec![0; n]` tocando páginas nuevas. A 60 fps son
**23 ms de trabajo inútil por cada segundo de emisión**.

**Arreglo.** Pool de frames reutilizables (plan 005). Un frame reciclado no
vuelve a tocar páginas ni a poner nada a cero.

### 3.3 Un bucle de lectura mal formado va 5× más lento que la memoria

**Síntoma.** Leer los 8,29 MB de un frame 1080p con
`row.iter().copied().map(u64::from).sum()` tarda 1,99 ms (4,2 GB/s), mientras que
escribirlos con `fill()` tarda 410 µs (20,2 GB/s).

**Causa.** La acumulación en un único registro crea una cadena de dependencias
que impide vectorizar.

**Estado.** No es un defecto: es la medición que fija la regla. Todo bucle por
píxel nuevo se escribe pensando en vectorizar, y este es el número que justifica
por qué. Anotado en CLAUDE.md §4.4.

---

## 4. Cómo se añade una entrada

1. Se mide **antes** de tocar nada, con `criterion` o con un contador del propio
   pipeline.
2. Se anota la hipótesis, no solo el resultado.
3. Se mide **después** y se apunta el cambio con su significancia (`criterion`
   la da: `p < 0.05`).
4. Si el experimento sale negativo, **también se anota**. Un experimento fallido
   documentado ahorra que otro lo repita.
