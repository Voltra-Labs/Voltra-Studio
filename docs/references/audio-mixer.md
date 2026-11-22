# Estado del arte: el mezclador de audio

Referencia para el plan 012. `docs/references/audio.md` cubre cómo se
**representan** las muestras; `docs/references/obs-studio.md` §3, el pipeline
*pull* y el buffering de ~1 s. Este documento cubre lo que pasa en medio: **cómo
libobs suma fuentes con volumen, balance y silencio**, leído en su código y no
en su documentación.

Fuentes (rama `master`, noviembre de 2026):

- [`libobs/obs-audio.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-audio.c) — `mix_audio`, la suma.
- [`libobs/obs-source.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-source.c) — `apply_audio_volume`, `apply_audio_actions`, `get_source_volume`, `process_audio_balancing`, `obs_source_set_volume`.
- [`libobs/media-io/audio-io.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/media-io/audio-io.c) — `input_and_output`, `clamp_audio_output`.

---

## 1. La suma

`mix_audio` es el bucle más simple posible, y es correcto que lo sea:

```c
for (size_t mix_idx = 0; mix_idx < MAX_AUDIO_MIXES; mix_idx++)
    for (size_t ch = 0; ch < channels; ch++)
        while (aud < end)
            *(mix++) += *(aud++);
```

Plano a plano, en float, sin recortar. Dos detalles:

- **`start_point`.** Si la fuente empieza a mitad del bloque, se suma a partir
  de la muestra que le corresponde por marca de tiempo. Es la alineación de §3
  de la referencia de OBS, y vive en la suma misma.
- **El volumen ya viene aplicado.** La suma no multiplica: cada fuente ha
  escalado antes su propio `audio_output_buf`. Son dos pasadas por muestra
  —escalar y sumar— donde una bastaría.

## 2. El volumen: acciones con marca de tiempo

`obs_source_set_volume` no toca el volumen. Encola una `audio_action`
(`AUDIO_ACTION_VOL`, `_MUTE`, `_PTT`, `_PTM`) con `os_gettime_ns()`, **bajo un
mutex** (`audio_actions_mutex`):

```c
struct audio_action action = {.timestamp = os_gettime_ns(), .type = AUDIO_ACTION_VOL, .vol = volume};
pthread_mutex_lock(&source->audio_actions_mutex);
da_push_back(source->audio_actions, &action);
pthread_mutex_unlock(&source->audio_actions_mutex);
```

En el hilo de audio, `apply_audio_actions` consume las acciones que caen dentro
del bloque y construye un **array de volumen por muestra** (`vol_data[1024]`):
el valor viejo hasta la muestra exacta del cambio, el nuevo desde ahí. Luego
`multiply_vol_data` multiplica cada canal por ese array.

Lo que eso significa:

1. **Es exacto a la muestra.** Un cambio ocurre en el instante en que se pidió,
   no al principio del bloque siguiente. Es la virtud del diseño.
2. **Es un escalón.** Entre una muestra y la siguiente, la ganancia salta del
   valor viejo al nuevo sin transición. Un salto grande en mitad de una onda es
   un **clic**, y un fader arrastrado son muchos escalones pequeños: el
   *zipper noise* clásico. Silenciar (`AUDIO_ACTION_MUTE`) es el caso extremo:
   de 1 a 0 en una muestra.
3. **El hilo de audio toma un mutex.** Una vez por fuente y bloque
   (`apply_audio_volume` mira si hay acciones), y otra más cuando las hay
   (`apply_audio_actions` las consume). Si la UI está dentro de `obs_source_set_volume` en ese momento, el
   hilo de audio espera a la UI. Además `da_push_back` puede reasignar, así que
   la sección crítica que el audio espera incluye un `realloc`.
4. **`da_erase` en mitad del bucle** desplaza el array por cada acción
   consumida: coste cuadrático en el número de acciones pendientes. Irrelevante
   con un fader, no con una automatización.

`get_source_volume` resuelve el resto: silencio si la fuente está deshabilitada,
muda, o en push-to-talk/push-to-mute, y redondea a exactamente 0 o 1 los
volúmenes a menos de 0,0001 de ellos para tomar los atajos (`vol == 1.0f` no
toca nada; `vol == 0.0f` hace `memset`).

## 3. El balance: una discontinuidad en el centro

`process_audio_balancing` se aplica **al recibir** el audio de la fuente
(`process_audio`), no al mezclar, y solo a fuentes estéreo. El balance va de 0
(izquierda) a 1 (derecha), con 0,5 en el centro, y la ley por defecto es la del
seno:

```c
data[0][frame] = data[0][frame] * sinf((1.0f - balance) * (M_PI / 2.0f));
data[1][frame] = data[1][frame] * sinf(balance * (M_PI / 2.0f));
```

Y la condición que la activa:

```c
if (!mono_output && source->sample_info.speakers == SPEAKERS_STEREO &&
    (source->balance > 0.51f || source->balance < 0.49f))
    process_audio_balancing(source, frames, source->balance, OBS_BALANCE_TYPE_SINE_LAW);
```

En el centro exacto, la ley del seno da `sin(π/4) = 0,707` a cada canal:
**−3 dB**. OBS lo evita saltándose el balance entre 0,49 y 0,51, donde la
ganancia es 1. Pero en 0,51 la ley ya se aplica y da 0,696 y 0,718: **la fuente
cae unos 3 dB de golpe** al mover el control un 1 % fuera del centro, y vuelve a
subir al regresar. La ley del seno es una ley de **paneo** (repartir una señal
mono entre dos altavoces a potencia constante) aplicada como **balance** (atenuar
un lado de una señal que ya es estéreo), y la banda muerta tapa el desajuste en
el punto donde más se nota sin quitarlo.

Hay además dos costes menores: `sinf` está dentro del bucle por muestra —con
`balance` constante el compilador puede sacarlo, pero no está garantizado—, y
el balance es una tercera pasada sobre las muestras, separada del volumen y de
la suma.

## 4. El recorte y los NaN

`input_and_output` pone a cero los búferes de las seis mezclas, pide la mezcla
y llama a `clamp_audio_output`, que por cada muestra de cada mezcla activa:

```c
val = (val == val) ? val : 0.0f;   // NaN -> 0
val = (val > 1.0f) ? 1.0f : val;
val = (val < -1.0f) ? -1.0f : val;
```

Antes copia la mezcla sin recortar a `buffer_unclamped`, para las salidas que
admiten float por encima de la unidad (`allow_clipping`).

Dos lecciones:

- **Se recorta una vez, al final.** Es exactamente el principio de
  `docs/references/audio.md` §2, y es correcto.
- **El NaN se limpia después de sumar.** Una fuente que entrega un NaN —un
  filtro con un estado corrupto, un decodificador roto— contamina la suma de
  esa muestra, y la limpieza la convierte en silencio **para todas las
  fuentes**. El fallo de una fuente se lleva el audio de las demás, en las
  muestras afectadas. Un `inf` es peor: no es NaN, así que sobrevive a la
  limpieza, y el recorte lo convierte en fondo de escala para la mezcla entera
  mientras dure.

## 5. Mezclas múltiples

Cada fuente lleva una máscara `audio_mixers` de seis bits, y el volumen se
aplica a los búferes de cada mezcla en la que participa. Es lo que permite
mandar a la grabación una mezcla distinta de la del streaming. El coste es
lineal: seis búferes por fuente y seis sumas.

---

## 6. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:**

- La suma en float planar, plano a plano, sin recortar.
- Recortar **una sola vez**, al convertir para la salida.
- Convertir NaN en silencio, en vez de dejar que llegue a un codificador.
- El vocabulario del usuario: volumen, balance, silencio por fuente.

**Qué hacemos distinto:**

1. **Sin mutex entre la UI y el hilo de audio.** Los parámetros de cada pista
   son atómicos: la UI escribe, el mezclador lee una vez por bloque, y ninguno
   espera al otro. Es CLAUDE.md §4.3 aplicado literalmente, y quita la
   dependencia de libobs en la que un `realloc` de la UI puede retrasar al
   audio.
2. **Rampa en vez de escalón.** Cada cambio de ganancia —volumen, balance o
   silencio— se aplica como una rampa lineal a lo largo de un bloque (21,3 ms a
   48 kHz). Se pierde la exactitud a la muestra de OBS, que es invisible para un
   fader movido a mano, y se gana lo que sí se oye: no hay clic al silenciar ni
   *zipper* al arrastrar. Es lo que hace toda estación de audio digital.
3. **Balance y paneo son leyes distintas.** Una fuente estéreo en una mezcla
   estéreo recibe **balance**, que vale exactamente 1 en el centro y solo atenúa
   el lado contrario: continuo, sin banda muerta y sin el salto de 3 dB. Una
   fuente mono en una mezcla estéreo recibe **paneo** a potencia constante, que
   sí cae 3 dB en el centro porque ese es su propósito.
4. **El NaN se aísla antes de sumar, por pista.** Una pista con una muestra no
   finita pierde su bloque y se cuenta como fallo suyo; las demás suenan
   intactas. CLAUDE.md §3: un fallo de una fuente nunca tumba el pipeline.
5. **Ganancia y suma en una pasada.** `mezcla += muestra × ganancia`, con el
   balance plegado en la ganancia de cada canal. OBS da tres pasadas (balance al
   recibir, volumen, suma).
6. **Una mezcla por mezclador, por ahora.** Las mezclas múltiples se adoptan,
   pero no en el primer paso: son un mezclador por mezcla o una matriz de
   envíos, y esa decisión se toma cuando haya dos salidas que la pidan.
