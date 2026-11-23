# 019 — Grabación larga: deriva y memoria medidas

- **Fase:** 4 (criterio de salida)
- **Estado:** en curso (2026-11-23) — ejecutado sin esperar revisión del plan,
  por indicación expresa del responsable ("no me preguntes, dale").

## Objetivo

Demostrar el criterio de salida de la fase 4 con una medida: `voltra record`
graba minutos de H.264 + AAC a 29,97 fps **sin deriva A/V** —la sincronía
comprobada en cada segundo del fichero— y **sin crecimiento de memoria** —lo
vivo, contado por el asignador, igual al final que al acabar el arranque—.

## Fuera de alcance

- **Fotogramas B y el vídeo anclado al reloj de audio**, que el ROADMAP lista
  en la fase 4. Lo primero no hace falta para el criterio; lo segundo solo
  tiene sentido con un reloj de captura real, que llega con la fase 5. Pasan
  allí, anotados.
- Grabaciones de horas en CI: el test de CI dura minutos de grabación; la de
  una hora se mide una vez y se anota.
- Buscar fugas por pila de creación (un perfilador de memoria).

## Referencia

`docs/references/long-recording.md`: el contador de `bmalloc` de OBS y su
"Number of memory leaks", el residente por `/proc/self/statm`, el *tracer*
`leaks` de GStreamer.

**Qué adoptamos:** un contador atómico siempre encendido en el asignador, y el
residente del sistema operativo para lo que el contador no ve.

**Qué mejoramos:** bytes además de bloques; muestras durante la grabación, no
solo al salir; el contador en el `#[global_allocator]`, que ve también las
dependencias.

## Diseño

### `heap`: el asignador que cuenta

En `voltra-cli` (el binario es quien elige el asignador global):

```rust
pub struct Counting;                 // envuelve `System`
unsafe impl GlobalAlloc for Counting { ... }

#[derive(Debug, Clone, Copy)]
pub struct HeapStats { pub live_bytes: u64, pub live_blocks: u64, pub allocations: u64 }
pub fn stats() -> HeapStats;
pub fn resident_kib() -> Option<u64>;   // VmRSS de /proc/self/status; None fuera de Linux
```

- Tres `AtomicU64` con orden `Relaxed`: son contadores, no publican datos.
  `realloc` cuenta como una asignación y ajusta los bytes vivos por la
  diferencia.
- El `unsafe` es el de `GlobalAlloc`: cada método delega en `System` con los
  mismos argumentos. `// SAFETY:` en cada bloque y un test que ejercita los
  cuatro caminos.
- `resident_kib` lee un fichero; no es código de plataforma con dependencias,
  así que va tras `#[cfg(target_os = "linux")]` sin feature, y fuera de Linux
  devuelve `None`. Desviación consciente de §2 (la regla pide feature): una
  feature para leer un fichero de texto no aísla nada.

### `voltra record --report-every SECONDS`

Cada `SECONDS` de **tiempo de grabación** (no de reloj), una línea en `stderr`:

```text
report at=60 heap_bytes=1234567 heap_blocks=345 allocations=67890 resident_kib=35448 queued=3
```

Clave=valor, para que un test o un script la lea sin ambigüedad. Leer el
residente es una llamada al sistema por informe: fuera del bucle por fotograma.
Sin la opción, el resumen final añade el pico de lo vivo y el residente.

### El test largo

`tests/record.rs`, con la feature `ffmpeg`: tres minutos a 160×90 y 29,97 fps,
H.264 + AAC, informes cada 10 s.

- **Sincronía en cada segundo.** A 29,97 fps el destello cae en el primer
  fotograma desde el segundo, hasta 33 ms después: el test anterior, que
  comparaba destello con pitido, no vale. Se miden por separado:
  - el error del audio, pitido − segundo: constante (la muestra de fase cero,
    más lo que AAC desplaza) en los 179 segundos medidos;
  - el error del vídeo, destello − instante exacto del primer fotograma desde
    el segundo: dentro de ±0,5 ms, la resolución de las marcas de Matroska.
  Un reloj que acumulara el redondeo —33 ms por fotograma en vez de
  33,366… ms— se iría 11 ms por segundo; uno en `f64` acumulado, menos, pero
  crecería.
- **Memoria.** Los bytes vivos del último informe no superan los del informe a
  los 10 s (el arranque ha terminado: x264 lleva su cola llena, el pool del
  entrelazador está en su pico). El residente, como mucho 1 MiB por encima.

Y uno sin `ffmpeg` que comprueba los informes en una grabación cruda corta.

## Pasos

1. **Documentos**: referencia y este plan.
2. **`heap` y `--report-every`**, con sus tests.
3. **El test largo y la medida de una hora**: cifras en `PERFORMANCE.md`,
   ROADMAP y CLAUDE.md; la fase 4 se cierra si el criterio se cumple.

## Verificación

- El contador ve una asignación, una reasignación y una liberación, con sus
  bytes.
- El test largo, en el trabajo `encode` de CI.
- Una grabación de 60 minutos, medida una vez: bytes vivos y residente al
  principio y al final, error de sincronía en el primer y el último segundo.

**Rendimiento:** el coste del contador en `voltra record` (tiempo de una
grabación con y sin él) y las asignaciones por segundo en régimen estable,
anotados.

## Riesgos

- **Algo crece.** Es lo que se quiere saber. Si es nuestro, se arregla en este
  plan si es pequeño; si no, se anota como deuda y la fase no se cierra.
- **El residente oscila** por el asignador del sistema. Por eso el criterio
  estricto es el de lo vivo, y el del residente es una cota.
- **Tres minutos en CI** cuestan: a 160×90, unos 6 s de grabación y lo que
  tarde FFmpeg en decodificar. Si pasa de un minuto, se baja la duración y se
  dice.
