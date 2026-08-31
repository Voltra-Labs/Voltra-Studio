# Registro de rendimiento

Este documento es el estado medible del proyecto: qué se ha medido, cuánto
cuesta, y qué está pendiente de arreglar. **Ninguna entrada se escribe sin
número.** Se actualiza al cerrar cada plan que toque el camino caliente.

Reproducir todo:

```bash
cargo bench -p voltra-core
cargo bench -p voltra-render
# El pipeline completo se mide solo: rendering y métricas por etapa a stderr.
cargo run --release -p voltra-cli -- render -o /dev/null --size 1920x1080 --frames 60
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
| **Reciclar frame BGRA 1080p (pool)** | **13,3 ns** | **0,00008 %** | 005 |
| Reciclar frame BGRA 1080p poniéndolo a cero | 344 µs | 2,1 % | 005 |
| Asignar frame I420 1080p | 133 µs | 0,8 % | 003 |
| Escribir todas las filas, BGRA 1080p | 410 µs | 20,2 GB/s | 003 |
| Leer todas las filas, BGRA 1080p | 1,99 ms | 4,2 GB/s | 003 |
| **Componer escena de 5 items, 1080p** | **33,0 ms** | **199 %** | 008 |
| Componer una capa a pantalla completa 1:1 | 11,5 ms | 69 % | 008 |
| Componer escalado a pantalla completa, bilineal | 36,9 ms | 222 % | 008 |
| **BGRA → NV12, 1080p** | **4,11 ms** | **24,8 %** | 004 |
| BGRA → I420, 1080p | 4,18 ms | 25,2 % | 004 |
| BGRA → NV12, 720p | 1,87 ms | 11,3 % | 004 |
| Escribir un frame I420 1080p a Y4M, a fichero | 3,91 ms | 23,5 % | 009 |
| Escribir un frame I420 1080p a Y4M, a `/dev/null` | 4 µs | 0,02 % | 009 |
| **Pipeline completo 1080p60, bilineal, a fichero** | **25,96 ms** | **156 %** | 009 |
| Pipeline completo 1080p60, vecino cercano, a fichero | 13,33 ms | 80 % | 009 |
| Pipeline completo 720p60, bilineal, a fichero | 10,72 ms | 64 % | 009 |

Audio, por bloque de **1024 muestras estéreo** — la unidad que mezcla libobs,
21,3 ms a 48 kHz. El porcentaje es sobre ese bloque, no sobre el frame de vídeo:

| Operación | Coste | % de 21,3 ms | Plan |
|---|---|---|---|
| Decodificar `f32` planar | 73,5 ns | 0,0003 % | 011 |
| Decodificar `i16` planar | 237 ns | 0,001 % | 011 |
| Decodificar `f32` intercalado | 368 ns | 0,002 % | 011 |
| Decodificar `i16` intercalado | 782 ns | 0,004 % | 011 |
| Codificar `f32` planar | 57,9 ns | 0,0003 % | 011 |
| Codificar `f32` intercalado | 335 ns | 0,002 % | 011 |
| **Codificar `i16` planar** | **1,91 µs** | 0,009 % | 011 |
| **Codificar `i16` intercalado** | **2,19 µs** | 0,010 % | 011 |
| Asignar búfer 1024 estéreo | 76,7 ns | 0,0004 % | 011 |
| Silenciar búfer 1024 estéreo | 44,3 ns | 0,0002 % | 011 |

Mezcla, por el mismo bloque de 1024 muestras estéreo:

| Operación | Coste | % de 21,3 ms | Plan |
|---|---|---|---|
| Mezclar 1 pista, ganancia asentada | 1,31 µs | 0,006 % | 012 |
| Mezclar 4 pistas | 2,57 µs | 0,012 % | 012 |
| **Mezclar 8 pistas** | **4,16 µs** | **0,020 %** | 012 |
| Mezclar 16 pistas | 7,78 µs | 0,037 % | 012 |
| Mezclar 8 pistas, todas en rampa | 14,7 µs | 0,069 % | 012 |
| Mezclar 1 pista, con medidor y balance | 5,80 µs | 0,027 % | 013 |
| Mezclar 4 pistas, con medidor y balance | 14,6 µs | 0,069 % | 013 |
| **Mezclar 8 pistas, con medidor y balance** | **25,9 µs** | **0,12 %** | 013 |
| Mezclar 16 pistas, con medidor y balance | 48,3 µs | 0,23 % | 013 |
| Mezclar 8 pistas en rampa, con medidor | 29,3 µs | 0,14 % | 013 |

Dos observaciones, ninguna accionable todavía:

1. **Codificar a `i16` cuesta 2,8× lo que decodificarlo.** La sujeción al rango
   entero es dos comparaciones y un `as` por muestra, y no está en la lectura.
2. **Intercalar cuesta 3–5× lo que copiar planos**, que es exactamente la
   transposición que `docs/references/audio.md` §2 anuncia como el precio del
   formato planar.

Ninguna se optimiza: el peor caso es el **0,01 % del bloque**. Están anotadas
para que nadie las descubra otra vez creyendo que ha encontrado algo.

Las cuatro últimas son del pipeline entero —componer, convertir y escribir— con
la escena de demostración de `voltra render`, no de un microbenchmark. Es la
primera medida de extremo a extremo del proyecto.

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

### 3.2 Asignar un frame cuesta 387 µs — RESUELTO (plan 005)

**Síntoma.** `VideoFrame::new` a 1080p BGRA tardaba 387 µs, ~46 µs por megabyte:
el `vec![0; n]` tocando páginas nuevas. A 60 fps, **23 ms de trabajo inútil por
cada segundo de emisión**.

**Arreglo aplicado.** `FramePool` (plan 005). El ciclo adquirir/devolver cuesta
**13,3 ns**, 28 400× menos. En régimen estacionario no se asigna nada.

**Matiz que conviene no olvidar.** `acquire_zeroed()` cuesta 344 µs, apenas un
9 % menos que asignar de cero: los dos caminos escriben los 8,29 MB enteros, y
ambos lo hacen al ancho de banda de la memoria. **El ahorro del pool no viene de
reutilizar la asignación sino de no escribir el búfer.** Toda etapa del pipeline
debe sobrescribir el frame completo; la que pinte encima de lo anterior pierde
el beneficio entero.

### 3.4 El compositor CPU está 8× por encima de su presupuesto

**Síntoma.** Una escena de 5 items a 1080p tarda 33,0 ms; el presupuesto de
composición son 4 ms. Incluso una sola capa opaca a escala 1:1 cuesta 11,5 ms.

**La medida que lo pone en contexto.** Esa capa 1:1 opaca es semánticamente una
copia de memoria. El `memcpy` de sus 8,29 MB cuesta 410 µs (§2). Estamos **28×
por encima del `memcpy` equivalente**.

**Ya aplicado — 9,6× acumulado**, en tres rondas medidas (detalle en el plan
008):

| Cambio | Ganancia |
|---|---|
| Coordenadas en punto fijo 16.16 en vez de `f32` con casts saturantes | −54 % |
| `clamp` sin ramas y atajo para píxeles opacos | −28 % |
| Sustituir bilineal por vecino cercano cuando el mapeo es 1:1 (bit a bit idéntico) | −69 % |

**Qué queda, si alguna vez hace falta un respaldo CPU usable:**

| Opción | Ganancia estimada | Coste |
|---|---|---|
| Copia directa de filas cuando el item está alineado, es opaco y usa `Normal` | La capa de fondo pasaría de 11,5 ms a ~0,5 ms | ~20 líneas y sus tests |
| Orden de canales como parámetro de tipo, para leer el píxel de una palabra en vez de cuatro bytes con índices en tiempo de ejecución | Sin medir; los índices variables impiden la carga de 32 bits | Duplica las instanciaciones a 28 |
| SIMD explícito | 2–4× | `unsafe`, plan propio |

**Decisión.** Ninguna se hace ahora. **Ninguna cambia el veredicto**: aunque la
copia de filas dejara la escena de 5 items en ~22 ms, seguiría siendo 5× por
encima. El ADR 0001 queda confirmado con datos: **la GPU no es una mejora
opcional del compositor, es un requisito**. El camino CPU cumple su papel —
correcto, portable, verificable en CI, y oráculo de las imágenes doradas con las
que se validará el backend acelerado— y ahí se queda.

### 3.6 El backend GPU existe, pero nadie lo ha medido — PENDIENTE DE HARDWARE

**Estado.** El plan 010 dejó `GpuCompositor` funcionando y verificado contra el
camino CPU: quince tests de paridad, exactitud donde puede haberla y tolerancia
justificada donde no.

**Lo que no hay: un solo número.** La verificación corre sobre **lavapipe**, el
rasterizador software de Mesa. Es el mismo código y los mismos píxeles, pero lo
ejecuta una CPU. Un tiempo medido ahí describe a llvmpipe y no dice nada sobre
una GPU, así que no se anota ninguno: sería exactamente la "opinión disfrazada
de optimización" que prohíbe §4.8.

**Qué hace falta para cerrar §3.4 y la fase 2.** Correr en hardware real, con
esta escena y este presupuesto:

```bash
sudo apt-get install -y mesa-vulkan-drivers   # solo para CI sin GPU
cargo run --release -p voltra-cli --features gpu -- \
    render -o /dev/null --size 1920x1080 --frames 300 --gpu
```

El resumen avisa por stderr cuando el adaptador es software, para que nadie
copie aquí un número que no lo es. La primera composición incluye la compilación
de los pipelines, así que se mide a partir de la segunda.

### 3.7 `mul_add` sin FMA en hardware cuesta 12× — RESUELTO (plan 012)

**Síntoma.** La primera versión del mezclador tardaba **50,0 µs** en sumar 8
pistas de 1024 muestras estéreo. Son 16 384 multiplicaciones-acumulaciones: unos
3 ns cada una, cuando un bucle así debería estar muy por debajo del nanosegundo.

**Causa.** El bucle usaba `f32::mul_add`, que garantiza **un solo redondeo**. Esa
garantía necesita una instrucción FMA, y FMA no está en la línea base de
`x86_64` —llegó con AVX2—, así que sin `target-feature=+fma` el compilador no
puede emitirla: emite una **llamada a la función `fmaf` de libm**. Una llamada a
función por muestra, que además impide vectorizar el bucle entero.

**Arreglo.** `a * b + c` en vez de `a.mul_add(b, c)`. La precisión extra de un
redondeo único no vale nada aquí: las muestras ya vienen de una conversión con
pérdida y van a otra.

| Caso | Antes | Después | Mejora |
|---|---|---|---|
| 8 pistas, asentado | 50,05 µs | 4,07 µs | **12,3×** |
| 16 pistas, asentado | 100,2 µs | 7,53 µs | 13,3× |
| 8 pistas, en rampa | 195,5 µs | 59,6 µs | 3,3× |

**Segunda ronda, sobre la rampa.** Partir el bloque en el tramo que la rampa
recorre de verdad y el resto ya asentado quita la rama de "¿he llegado?" de
ambos bucles: **59,6 → 14,7 µs, otro 4×**. Con una rampa de 10 ms en un bloque
de 21,3 ms, más de la mitad del bloque estaba pasando por aritmética de rampa
sin necesitarla.

**Dónde más mirar.** `mul_add` aparece también en `U8Depth::encode`
(`crates/voltra-core/src/audio/convert.rs`), en el camino de 8 bits sin signo.
Es una por muestra y ese formato es raro; queda anotado, sin medir y sin tocar.
En el camino de píxeles no aparece: el muestreo del plan 008 es entero.

### 3.8 Medir la señal cuesta 6,3×, y dos intentos de arreglarlo salieron peor

**Síntoma.** Añadir pico y RMS al bucle de mezcla llevó 8 pistas de **4,16 µs a
26,3 µs**. El plan 013 había estimado que se duplicaría; se multiplicó por 6,3.

Son tres operaciones más por muestra —valor absoluto, comparación y
multiplicación-acumulación— sobre un bucle que hacía dos. La estimación estaba
mal por no contar que el bucle original vectoriza y el medidor lo impide.

**Intento 1: pico sin ramas.** El plan 008 midió que sustituir un `clamp` con
ramas por uno sin ramas daba −28 %, así que la hipótesis era que
`self.peak.max(magnitude)` batiría a `if magnitude > self.peak`.

**Salió 1,8× peor: 26,3 → 46,5 µs.** `f32::max` en Rust lleva la semántica NaN
de IEEE 754 —tiene que decidir qué devolver cuando un operando es NaN— y no
compila a una sola `maxss`. La lección del plan 008 **no se transfiere**: allí lo
que se quitaba era una cadena de `if/else if/else`, aquí lo que se metía era una
función con casos especiales.

**Intento 2: `mul_add` para la suma de cuadrados.** Salió **3× peor**
(26,3 → 78,2 µs), por exactamente la misma razón que ya está escrita en §3.7:
sin FMA en hardware es una llamada a `fmaf` por muestra.

Es la segunda vez que este proyecto cae en `mul_add`, dos planes después de
documentarlo. La conclusión práctica: **en un bucle por muestra o por píxel,
`mul_add` es un error salvo que se compile con `+fma` y se mida**. Está anotado
aquí y en el propio código.

**Decisión.** Se queda la versión con rama y sin `mul_add`, que es la más rápida
de las tres medidas. 25,9 µs es el **0,12 % del bloque** de 21,3 ms, y CLAUDE.md
§4.10 pide las métricas internas siempre encendidas: un medidor que hay que
activar es un medidor que nadie tiene activado cuando hace falta.

### 3.5 Escribir Y4M cuesta el 23 % del presupuesto, y es disco

**Síntoma.** Escribir un frame I420 1080p a un fichero cuesta 3,91 ms. A
`/dev/null` cuesta 4 µs.

**Causa.** No es CPU: es el ancho de banda que pide el formato. Y4M es vídeo
crudo, y 1080p60 en 4:2:0 son **187 MB/s** sostenidos. El escritor ya hace lo
único que puede hacer —un `write_all` por plano, sin asignar, sin copiar y sin
emitir el relleno del stride—; lo que queda es el disco.

**Decisión.** No se arregla, porque no está roto. Y4M es un formato de
desarrollo y de tubería (`voltra render -o - | x264 --demuxer y4m -`), y en
tubería el coste no aparece. Grabar de verdad es la **fase 4**: un codec baja
esos 187 MB/s a unos pocos MB/s, y ese es el arreglo. La entrada existe para que
nadie confunda el coste del formato con un problema del escritor.

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
