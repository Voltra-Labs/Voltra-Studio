# Estado del arte: medir una grabación larga

Referencia para el plan 019, el criterio de salida de la fase 4: **grabar
minutos sin deriva A/V ni crecimiento de memoria**. Lo que hay que saber medir
es cuánta memoria sigue viva y si crece, y si la sincronía se mantiene segundo a
segundo. Qué mide OBS de eso, y cómo.

Fuentes (rama `master` de OBS, noviembre de 2026):

- [`libobs/util/bmem.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/util/bmem.c) — el contador de asignaciones vivas.
- [`frontend/obs-main.cpp`](https://github.com/obsproject/obs-studio/blob/master/frontend/obs-main.cpp) — el informe de fugas al salir.
- [`libobs/util/platform-nix.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/util/platform-nix.c) — la memoria residente del proceso.
- [El *tracer* `leaks` de GStreamer](https://gstreamer.freedesktop.org/documentation/coretracers/leaks.html).

---

## 1. OBS cuenta bloques vivos, y lo dice al salir

Todo lo que `libobs` reserva pasa por `bmalloc`/`bfree`, que llevan la cuenta de
los bloques vivos con atómicos:

```c
static long num_allocs = 0;
...
os_atomic_inc_long(&num_allocs);   /* bmalloc, y brealloc con ptr == NULL */
...
os_atomic_dec_long(&num_allocs);   /* bfree */
...
long bnum_allocs(void) { return num_allocs; }
```

Al terminar el programa, `main` lo escribe en el registro:

```c
blog(LOG_INFO, "Number of memory leaks: %ld", bnum_allocs());
```

Es la cifra que cualquiera que haya leído un registro de OBS conoce. Tiene tres
límites:

1. **Solo ve lo que pasa por `bmalloc`.** Lo que reservan FFmpeg, x264 o un
   driver con su propio `malloc` no cuenta.
2. **Cuenta bloques, no bytes.** Una cola que crece reutilizando un búfer que se
   agranda con `brealloc` no cambia la cuenta.
3. **Solo al final.** Una cola que crece durante dos horas y se vacía al cerrar
   da cero fugas: el problema de una grabación larga no es lo que queda al
   salir, sino lo que crece mientras dura.

## 2. La memoria residente: `/proc/self/statm`

Para lo que el contador no ve, OBS pregunta al sistema operativo. En Linux,
`os_get_proc_resident_size` lee el segundo campo de `/proc/self/statm` —páginas
residentes— y lo multiplica por el tamaño de página:

```c
const char *statm_path = "/proc/self/statm";
...
return (uint64_t)statm.resident_size * sysconf(_SC_PAGESIZE);
```

El residente lo ve todo —Rust, C, pilas, código—, pero con ruido: el asignador
del sistema no devuelve cada página que se libera, y un pico deja el residente
alto aunque la memoria ya no se use. Sirve para acotar un crecimiento, no para
encontrarlo.

## 3. GStreamer: objetos vivos, por tipo

El *tracer* `leaks` (`GST_TRACERS=leaks`) lleva la cuenta de los `GstObject` y
`GstMiniObject` vivos —búferes, eventos, elementos— y al salir lista los que
quedan, por tipo y, si se pide, con la pila de su creación. También puede tomar
instantáneas durante la ejecución y comparar dos. Es la misma idea que la de
OBS, sobre los objetos del propio marco.

---

## 4. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:**

- Un contador atómico en el asignador, siempre encendido. Una suma relajada por
  asignación no se nota.
- El residente del sistema operativo, para lo que el contador no ve.

**Qué hacemos distinto:**

1. **Bytes y bloques**, no solo bloques: el crecimiento de un búfer que se
   reasigna también es crecimiento.
2. **Durante la grabación, no solo al final.** `voltra record` informa cada
   cierto tiempo de grabación, y el test exige que lo vivo no crezca entre el
   final del arranque y el final de la grabación.
3. **El contador está en el asignador global** (`#[global_allocator]`), no en
   una función que haya que acordarse de llamar: ve todo Rust, también las
   dependencias.
4. **El residente se lee de `/proc/self/status` (`VmRSS`, en KiB)** en vez de
   `statm` por el tamaño de página: así no hace falta `sysconf`, y con él una
   dependencia de `libc`, para una medida.

**Qué queda fuera:** la pila de creación de cada bloque —eso es un perfilador
de memoria (`heaptrack`, `valgrind --tool=massif`), y se usa cuando el contador
diga que algo crece.
