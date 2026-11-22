# Estado del arte: remuestreo de audio

Referencia para el plan 014. Cómo convierte libobs una fuente de 44,1 kHz a la
mezcla de 48 kHz, qué filtro usa realmente y qué calidad da, medida.

Fuentes:

- [`libobs/media-io/audio-resampler-ffmpeg.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/media-io/audio-resampler-ffmpeg.c) — el envoltorio de OBS: `audio_resampler_create`, `audio_resampler_resample`.
- [`libobs/obs-source.c`](https://github.com/obsproject/obs-studio/blob/master/libobs/obs-source.c) — `process_audio` y `reset_resampler`, dónde y cuándo se remuestrea.
- [`libswresample/options.c`](https://github.com/FFmpeg/FFmpeg/blob/master/libswresample/options.c) y [`libswresample/resample.c`](https://github.com/FFmpeg/FFmpeg/blob/master/libswresample/resample.c) — los valores por defecto y `build_filter`, el diseño del filtro.

Todo leído en noviembre de 2026, rama `master` de OBS y de FFmpeg.

---

## 1. Dónde remuestrea OBS

En la entrada de cada fuente. `process_audio` compara la frecuencia, el formato
y la disposición de lo que entrega la fuente con lo último que entregó; si
cambian, `reset_resampler` crea un remuestreador nuevo. Desde ahí, cada bloque
pasa por `audio_resampler_resample` antes de entrar en las colas de la mezcla.
Ocurre en el hilo de la fuente, no en el de la mezcla.

El remuestreador es **libswresample** (FFmpeg), creado con
`swr_alloc_set_opts2(..., 0, NULL)`: **sin ninguna opción**, así que todo lo que
sigue son los valores por defecto de swr.

Dos detalles del envoltorio:

- **El búfer de salida se reasigna** cuando la estimación de muestras crece:

  ```c
  if (estimated > rs->output_size) {
      if (rs->output_buffer[0])
          av_freep(&rs->output_buffer[0]);
      av_samples_alloc(rs->output_buffer, NULL, rs->output_ch, estimated, rs->output_format, 0);
      rs->output_size = estimated;
  }
  ```

  Es raro en régimen estacionario —el tamaño se estabiliza—, pero un bloque
  mayor que los anteriores asigna en el camino del audio.
- **El retardo del filtro se descuenta de la marca de tiempo**:
  `*ts_offset = swr_get_delay(context, 1000000000)`, en nanosegundos, y
  `source_output_audio_data` lo resta (`in.timestamp -= source->resample_offset`). La salida queda alineada con la entrada en el
  tiempo.

Un hallazgo lateral, que no es de remuestreo pero vive en el mismo fichero:
**una fuente mono en una salida estéreo se duplica con ganancia 1 en los dos
canales** (`swr_set_matrix` con la fila `{1, 1}`). El mezclador de Voltra
(plan 012) la reparte con paneo a potencia constante: −3 dB por lado en el
centro. Un micrófono mono suena 3 dB más bajo aquí que en OBS. Es una decisión
consciente del plan 012, pero no estaba escrito que difiriera de OBS; queda
anotado para cuando se importen escenas.

## 2. El filtro de swr por defecto

De `options.c` y `resample_init`:

| Parámetro | Valor | Qué hace |
|---|---|---|
| `filter_size` | 32 | coeficientes por fase al subir de frecuencia |
| `phase_shift` | 10 | hasta 1024 fases |
| `exact_rational` | 1 | si la razón reducida cabe en 1024 fases, se usa exacta |
| `linear_interp` | 1 | interpolar entre fases cuando no es exacta |
| `cutoff` | 0,97 | corte relativo al Nyquist menor |
| `filter_type` | Kaiser | ventana |
| `kaiser_beta` | 9 | forma de la ventana |

Y en `build_filter`:

```c
/* if upsampling, only need to interpolate, no filter */
if (factor > 1.0)
    factor = 1.0;
```

con `factor = min(out_rate × cutoff / in_rate, 1)`. **Al subir de frecuencia el
corte del 0,97 no se aplica**: el sinc corta justo en el Nyquist de la
entrada. El filtro solo interpola; no hace nada contra las imágenes que caen
justo por encima de ese Nyquist.

La normalización usa la suma de la **fase 0** para todas las fases
(`if (!ph) norm += y`). Las demás fases quedan con ganancia en continua
ligeramente distinta de 1.

Para 44,1 → 48 kHz la razón reducida es 160/147: 160 fases exactas, sin
interpolación entre fases. Para 48 → 44,1, 147/160, con
`ceil(32 / 0,891)` = 36 coeficientes por fase.

## 3. Lo que da ese filtro, medido

Emulado fuera del proyecto en `numpy` con la misma fórmula de `build_filter`
—mismo sinc, misma ventana, misma normalización, `f64`—, no medido sobre
libswresample compilada. Tonos de amplitud 1; la ganancia y el error salen de
ajustar por mínimos cuadrados una sinusoide a la frecuencia exacta, y el error
es todo lo que queda: imágenes, aliasing y redondeo.

**44,1 → 48 kHz:**

| Tono | swr por defecto | 128 coef., β = 12, corte 0,95 |
|---|---|---|
| 1 kHz | 0,000 dB, error −107 dB | 0,000 dB, −137 dB |
| 10 kHz | 0,000 dB, −101 dB | 0,000 dB, −136 dB |
| 18 kHz | 0,000 dB, −98 dB | 0,000 dB, −126 dB |
| **20 kHz** | **−0,50 dB, error −25 dB** | **0,000 dB, −133 dB** |

**48 → 44,1 kHz:**

| Tono | swr por defecto | 128 coef., β = 12, corte 0,95 |
|---|---|---|
| 1 kHz | 0,000 dB, −109 dB | 0,000 dB, −141 dB |
| 18 kHz | −0,01 dB, −100 dB | 0,000 dB, −135 dB |
| **20 kHz** | **−1,28 dB**, −92 dB | **−0,017 dB**, −142 dB |
| **23 kHz** (por encima del Nyquist de salida) | **aliasing a −20 dB** | **−121 dB** |

Las dos filas en negrita son las que importan:

- **Subiendo, un tono de 20 kHz deja una imagen a 23,9 kHz solo 25 dB por
  debajo.** Es la consecuencia directa de cortar en el Nyquist exacto: la
  imagen del tono (44,1 − 20 = 24,1 kHz) cae en la banda de transición del
  filtro y se repliega a 23,9 kHz.
- **Bajando, un tono de 23 kHz se repliega a 21,1 kHz atenuado solo 20 dB.** Un
  contenido de 48 kHz por encima de 22 kHz —ruido de un convertidor, un
  sintetizador— entra en la banda audible de la salida.

Ninguna de las dos se oye fácilmente: están en el borde de lo audible y la
música rara vez tiene energía ahí. Pero la salida acaba en un codificador con
pérdidas, que gasta bits en lo que hay, y el aliasing por debajo de 22 kHz no
tiene arreglo después.

Lo que hemos comparado también tiene un coste, y es la otra mitad de la
decisión: 128 coeficientes son 4× los 32 de swr.

## 4. Resumen: qué adoptamos y qué mejoramos

**Qué adoptamos:**

- Polifase con **razón racional exacta**: para 44,1 ↔ 48 kHz, 160/147 fases
  sin interpolar. Cada muestra de salida cae en una posición exacta, así que no
  hay deriva por mucho que dure la emisión.
- Ventana de Kaiser y sinc enventanado, el diseño de swr.
- Salida alineada con la entrada en el tiempo: el retardo del filtro no se
  traslada a la marca de tiempo.

**Qué hacemos distinto:**

1. **Filtro más largo y con corte también al subir**: 128 coeficientes por
   fase, β = 12, corte al 0,95 del Nyquist menor en los dos sentidos. Banda
   pasante plana hasta 20 kHz, error por debajo de −120 dB y aliasing a −121 dB,
   frente a −25 y −20 dB en los dos casos peores de swr (§3). A cambio, 4× las
   multiplicaciones, que se miden.
2. **Cada fase normalizada a ganancia 1 en continua**, no todas por la suma de
   la fase 0: la continua y los graves salen exactos en cualquier fase.
3. **Ninguna asignación al remuestrear.** El búfer de trabajo se reserva al
   crear el remuestreador para el bloque máximo, y la salida es un
   `AudioBuffer` del llamante con capacidad comprobada.
4. **La alineación temporal es exacta por construcción**, no una resta del
   retardo: la muestra de salida *k* es la entrada interpolada en el instante
   *k* / *f*<sub>salida</sub>, con silencio antes del principio. La marca de tiempo de cada
   bloque de salida se calcula desde el índice absoluto, como todo reloj de
   Voltra.

**Qué queda fuera, a sabiendas:** razones que no caben en 1024 fases (ninguna
entre frecuencias estándar las necesita); la razón variable que hará falta
para corregir la deriva de reloj entre dispositivos, que es el plan de
sincronía; y la remezcla de disposiciones, que sigue siendo del mezclador.
