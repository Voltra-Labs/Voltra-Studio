# Estado del arte: del reloj de la fuente al de la mezcla

Referencia para el plan 015. Cómo lleva libobs el audio de cada fuente, que
llega cuando llega y con la marca de tiempo que trae, hasta el bloque de 1024
muestras que se mezcla en un instante concreto: el alisado de marcas de tiempo,
la colocación en la cola, el buffering global y lo que se descarta.

Fuentes:

- [`libobs/obs-source.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-source.c) — `source_output_audio_data`, `handle_ts_jump`, `source_output_audio_place`, `source_output_audio_push_back`.
- [`libobs/obs-audio.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-audio.c) — `audio_callback`, `ignore_audio`, `add_audio_buffering`, `set_fixed_audio_buffering`.
- [`libobs/obs-internal.h`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-internal.h) y [`libobs/obs.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs.c) — `MAX_TS_VAR` y `max_buffering_ticks`.

Todo leído en noviembre de 2026, rama `master`. `obs-studio.md` §3 ya resume
el modelo *pull*; esto baja al código.

---

## 1. Al entrar: alisar la marca de tiempo

`source_output_audio_data` recibe cada bloque de una fuente. Dos constantes
gobiernan lo que hace con su marca de tiempo:

```c
#define TS_SMOOTHING_THRESHOLD 70000000ULL   // 70 ms
#define MAX_TS_VAR             2000000000ULL // 2 s
```

1. **Dominio del reloj.** Si la marca de tiempo está a menos de 2 s del reloj
   del sistema (`os_gettime_ns`), se toma como "directa": la fuente ya habla en
   tiempo del sistema. Si no, `timing_adjust = os_time − timestamp` traslada el
   reloj privado de la fuente al del sistema, fijado con el primer bloque.
2. **Continuidad.** Se compara con la marca esperada, `next_audio_ts_min` (el
   final del bloque anterior):
   - diferencia **menor de 70 ms**: la marca se **sustituye por la esperada**.
     La fluctuación del dispositivo desaparece y el flujo queda contiguo;
   - diferencia **mayor de 2 s**: es un salto; `handle_ts_jump` reinicia la
     traducción de reloj y vacía la cola;
   - **entre 70 ms y 2 s**: se registra en el log de depuración y la marca se
     respeta tal cual. El bloque se coloca donde dice, y eso deja un hueco o
     pisa lo anterior.
3. Se suma el `sync_offset` del usuario y se resta el retardo del remuestreador.

La decisión de alisar es buena y la adoptamos: un dispositivo que entrega
cada 10 ms con ±2 ms de fluctuación no tiene huecos de verdad, y tratarlos como
tales llenaría la mezcla de clics.

## 2. La cola de cada fuente

Cada fuente tiene un `deque` por canal (`audio_input_buf`), protegido por
`audio_buf_mutex`, que toma tanto el hilo de la fuente al escribir como el de la
mezcla al leer.

- Si el bloque es contiguo, `source_output_audio_push_back` lo añade al final.
- Si no, `source_output_audio_place` lo coloca en la posición que da su marca de
  tiempo respecto a `audio_ts`, sobrescribiendo lo que hubiera y recortando lo
  que quede detrás. Si la marca es anterior al principio de la cola, la vacía.
- **Tope:** `MAX_BUF_SIZE` = 1000 × 1024 muestras por canal, unos 21 s a 48 kHz.
  Un bloque que lo excedería **se descarta sin aviso**:

  ```c
  /* do not allow the circular buffers to become too big */
  if ((buf_placement + size) > MAX_BUF_SIZE)
      return;
  ```

`deque_push_back` y `deque_place` hacen crecer el `deque` con `realloc` cuando
no cabe, y lo hacen con `audio_buf_mutex` tomado: el hilo de la mezcla, que
toma ese mismo mutex para leer, puede esperar a que termine una reasignación.

## 3. Al mezclar: el buffering global

`audio_callback` mezcla cada 1024 muestras un intervalo `[start, end)` de tiempo
del sistema. Si una fuente todavía no tiene datos para ese intervalo, libobs no
la deja fuera: **retrasa la mezcla entera**. `add_audio_buffering` calcula
cuántos bloques le faltan a esa fuente y los suma a `total_buffering_ticks`,
desplazando hacia atrás el intervalo que se mezcla:

```c
offset = ts->start - min_ts;
frames = ns_to_audio_frames(sample_rate, offset);
ticks = (int)((frames + AUDIO_OUTPUT_FRAMES - 1) / AUDIO_OUTPUT_FRAMES);
audio->total_buffering_ticks += ticks;
```

con un techo, `max_buffering_ticks`, que por defecto es **45 bloques**
(`obs.c`): unos 960 ms a 48 kHz.

Las propiedades del modelo:

- **El buffering solo crece.** No hay código que lo reduzca: una fuente que
  llegó tarde una vez deja a toda la emisión con más latencia hasta que se
  reinicia el audio. El log lo dice: *"adding %d milliseconds of audio
  buffering, total audio buffering is now %d milliseconds"*.
- **Lo paga todo el mundo.** El retraso es de la mezcla, no de la fuente que lo
  causó: un dispositivo Bluetooth lento retrasa también al micrófono USB.
- Hay una alternativa, `fixed_buffering`: la primera vez que hace falta
  buffering, salta directamente al máximo en vez de crecer poco a poco
  (`set_fixed_audio_buffering`).
- Una fuente que se queda **atrás** respecto al intervalo pierde lo viejo:
  `ignore_audio` descarta las muestras anteriores a `start` y la fuente se
  reengancha cuando las alcanza.

`obs-studio.md` §3 cita a los propios desarrolladores de OBS: este buffering
"introdujo una complejidad significativa".

## 4. Lo que no hace

- **No corrige la deriva entre relojes.** El remuestreador tiene razón fija:
  `audio-resampler-ffmpeg.c` nunca llama a `swr_set_compensation`. Una fuente
  cuyo reloj corre un 0,01 % más rápido entrega 4,8 muestras de más por segundo.
  El alisado las absorbe mientras el desfase acumulado no pase de 70 ms; cuando
  pasa, la marca deja de alisarse y la colocación por marca de tiempo corta o
  pisa. Un 0,01 % llega a 70 ms en unos 12 minutos.
- No ofrece métricas de la cola por fuente: cuánto margen tiene, cuántas veces
  se quedó vacía, cuánto se descartó. El log informa cuando el buffering crece.

---

## 5. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:**

- Alisar la marca de tiempo por debajo de un umbral, con 70 ms por defecto como
  OBS: la fluctuación no es un hueco.
- Colocar por marca de tiempo lo que no es contiguo: un hueco de verdad se
  rellena con silencio en su sitio, no se cierra.
- Descartar lo que llega cuando su instante ya se mezcló, como `ignore_audio`.
- Un tope a lo que puede acumular una fuente.

**Qué hacemos distinto:**

1. **Latencia explícita y fija**, elegida por quien conduce la mezcla, en vez
   de buffering que solo crece. Si una fuente no llega a tiempo, esa fuente se
   queda sin bloque —el mezclador la silencia con fundido y la cuenta—; las
   demás no esperan. Es el *modelo explícito de buffering* que `obs-studio.md`
   §6 apunta como mejora.
2. **Cola sin mutex y sin reasignar.** Un anillo de un productor y un
   consumidor, reservado al crearlo: el hilo de la fuente escribe y el de la
   mezcla lee sin que ninguno espere al otro. En código seguro: las muestras son
   atómicos con orden relajado, publicadas con un índice *release*, sin
   `unsafe`.
3. **El alisado ocurre antes del remuestreo.** Así el remuestreador del plan 014
   ve un flujo exactamente continuo y no se reinicia con cada fluctuación.
4. **Métricas por fuente siempre encendidas** (CLAUDE.md §4.10): margen hasta el
   siguiente tick, bloques vacíos, muestras tardías, huecos, solapes,
   desbordes. Son las que dicen si la latencia elegida basta.
5. **El tope se cuenta**, no se descarta en silencio.

**Qué queda fuera, a sabiendas:**

- **La traducción de relojes privados** (`timing_adjust`). Aquí todas las
  marcas de tiempo se asumen en un mismo reloj monótono, el que usa PipeWire y
  el que usará el motor. Una fuente con reloj propio —un fichero que empieza en
  cero— traduce el suyo antes de entregar.
- **La corrección de deriva** con remuestreo de razón variable: necesita
  dispositivos reales para medirla y vuelve con la captura (fase 5).
- **Atar el vídeo al reloj de audio**: necesita el pipeline de grabación
  (fase 4), donde la deriva A/V es el criterio de salida.
- El `sync_offset` por fuente: es una suma en la marca de tiempo, y llega con la
  interfaz que lo edita.
