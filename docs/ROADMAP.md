# Voltra Studio — Roadmap

Fases del producto. Cada fase se descompone en pasos pequeños con su plan en
`docs/plans/`. No se empieza una fase sin cerrar la anterior.

## Fase 0 — Fundaciones
- ADRs de las decisiones abiertas (render, plataforma, UI, encoding).
- Esqueleto del workspace, lints compartidos, perfiles de compilación, CI.
- **Criterio de salida:** `cargo test`, `clippy -D warnings` y `fmt` en verde en
  un contenedor headless.

## Fase 1 — Vocabulario y geometría (`voltra-core`)
- Tipos de frame, formatos de píxel, reloj racional, transformaciones, grafo de
  escena, traits `Source`/`Filter`/`Output`, sistema de propiedades.
- **Criterio de salida:** cobertura de tests sobre geometría, color y timing.

## Próximos pasos comprometidos

**La secuencia 007–010 está cerrada.** El objetivo era llegar al primer hito
visible en tres pasos, y se llegó; el cuarto trajo el backend acelerado.

| Plan | Qué dejó funcionando | Estado |
|---|---|---|
| **007** | Grafo de escena: `Scene`, `SceneItem`, orden de capas, visibilidad, bloqueo | Hecho |
| **008** | Compositor CPU: escena → frame, mezcla alfa, muestreo por mapeo inverso, imágenes doradas | Hecho, con el criterio de rendimiento incumplido y documentado |
| **009** | Salida Y4M desde `voltra render` | **Hecho. Primer hito visible.** |

Desde aquí, cada paso se puede *ver*:

```bash
voltra render -o - --frames 300 | ffplay -
voltra render -o demo.y4m --size 1920x1080 --frames 600
```

| **010** | Backend GPU `wgpu` tras el mismo trait, más `voltra render --gpu` | Hecho, verificado contra el camino CPU |

### Qué viene ahora

La fase 2 **sigue sin cerrarse, y por un motivo concreto**: su criterio de
salida es 1080p60 dentro del presupuesto, y eso es una medición. El backend GPU
ya existe y es correcto (plan 010), pero se ha verificado sobre un rasterizador
software, así que **no hay ninguna cifra de GPU** (`docs/PERFORMANCE.md` §3.6).
Cerrar la fase 2 no requiere escribir código: requiere una máquina con GPU y una
tarde.

Mientras tanto avanzó la **fase 3**. El plan 011 dejó el vocabulario de audio
—frecuencia, disposición de canales, formatos de borde y el búfer float planar—
en `voltra-core`, y el plan 012 el **mezclador** en `voltra-audio`: pistas con
volumen, balance/paneo y silencio, controladas desde otro hilo con atómicos, y
**sin asignar ni bloquear en `mix()`**, comprobado por un test que cuenta
asignaciones y por un lint que prohíbe los mutexes en la crate. 8 pistas en
rampa cuestan 3,24 µs por bloque de 21,3 ms.

**La fase 3 está cerrada.** Los planes 013 a 015 completaron el camino del
audio: **medidores** —pico, pico verdadero por BS.1770 y RMS, sin perder ningún
pico y sin mutex, con la balística de OBS como función pura—; **remuestreo**
polifase de razón exacta, plano hasta 20 kHz y con aliasing a −124 dB, frente a
−20 dB del filtro por defecto de swr que usa OBS; y la **entrada de audio** por
fuente, que alisa la fluctuación de las marcas de tiempo, remuestrea, coloca por
marca de tiempo y entrega a cada tick su bloque exacto, sin mutex ni
asignaciones. Diez segundos de una fuente a 44,1 kHz con ±5 ms de fluctuación,
entregados desde un hilo y mezclados a 48 kHz en otro, salen con un error de
−136 dB respecto al tono ideal.

**Decisión al cerrar** (2026-11-22): lo que quedaba bajo "sincronía A/V" se
reparte donde se puede medir.

- **La corrección de deriva entre relojes de dispositivos** pasa a la fase 5. Sin
  dispositivos no hay deriva que medir, y corregirla —remuestreo de razón
  variable guiado por el margen de la cola— sin una medida sería una opinión.
  Las métricas de margen de la entrada ya la enseñan.
- **Atar el vídeo al reloj de audio** pasa a la fase 4, cuyo criterio de salida
  ("sin deriva A/V") es exactamente esa medida, y que trae el pipeline donde
  hacerla.

**La fase 4 está cerrada.** El plan 016 dejó un muxer Matroska nativo que
escribe en flujo —válido en cada byte, así que un cuelgue deja una grabación
reproducible— y `voltra record`, que graba la escena con un tono mezclado por el
mezclador de la fase 3. FFmpeg lee el fichero, y la sincronía medida en él es de
una muestra, sin deriva. El plan 017 añadió el **encoder H.264** tras un trait
validado en papel contra x264, NVENC, VAAPI y VideoToolbox: x264 por
`libavcodec`, tras la feature `ffmpeg`, graba con la misma sincronía medida y
veinte veces menos espacio. El plan 018 añadió el **encoder AAC** y el
**entrelazado**: `voltra record --video h264 --audio aac` graba un fichero
comprimido en las dos pistas, con la sincronía medida en marcas reales a 0,35 ms
y sin deriva. El plan 019 midió el criterio de salida: **una hora de H.264 +
AAC a 29,97 fps** con el pitido de cada segundo a 354,2 µs de su segundo, del
primero al último, y el heap de Rust sin asignar nada en régimen estable. La
medida encontró un desfase constante de 1 ms en el encoder H.264, que contaba
los fotogramas desde el cero y no desde el primero, y lo corrigió.

Después: captura con PipeWire (fase 5), el momento en que el programa sirve para
algo, streaming (fase 6) e interfaz (fase 7).

Deuda de rendimiento pendiente en `docs/PERFORMANCE.md`.

## Fase 2 — Compositor
- Composición escena → frame con transformaciones, recorte, mezcla alfa y
  escalado de calidad. Camino CPU verificable + tests de imagen dorada.
- Backend GPU según el ADR de render.
- **Criterio de salida:** 1080p60 compuesto dentro del presupuesto, con
  benchmark publicado.

## Fase 3 — Audio
- Vocabulario: frecuencia, canales, formatos de borde, búfer float planar
  (plan 011, hecho).
- Mezclador multipista con volumen, balance/paneo y silencio (plan 012, hecho).
- Medidores: pico, pico verdadero, RMS y balística (plan 013, hecho).
- Remuestreo polifase de razón exacta (plan 014, hecho).
- Entrada de audio por fuente: alisado, remuestreo, colocación por marca de
  tiempo, latencia explícita (plan 015, hecho).
- **Criterio de salida:** mezcla sin asignaciones ni bloqueos en el callback.
  **Cumplido** (plan 012). **Fase cerrada** con el plan 015.

## Fase 4 — Salida a fichero
- Muxer Matroska nativo en flujo y `voltra record` con A/V sincronizado y
  medido por FFmpeg, aún sin comprimir (plan 016, hecho; ADR 0005).
- Encoder H.264 (x264 por `libavcodec`, tras la feature `ffmpeg`) y grabación
  comprimida con la misma sincronía medida (plan 017, hecho).
- Encoder de audio AAC y entrelazado de encoders con retardo, con un origen
  común para todas las pistas (plan 018, hecho).
- Memoria contada por el asignador y sincronía medida en cada segundo de una
  grabación larga: una hora, sin deriva ni crecimiento (plan 019, hecho).
- **Criterio de salida:** grabar una escena durante minutos sin deriva A/V ni
  crecimiento de memoria. **Cumplido** (plan 019).

Salen de la fase, con su razón (plan 019): **el vídeo anclado al reloj de
audio** va a la fase 5, porque con una escena sintética los dos relojes son el
mismo contador de muestras y no hay nada que anclar; **los fotogramas B** van a
la fase 6, donde el bitrate más importa.

## Fase 5 — Captura de plataforma
- `voltra-capture` y la entrega de la captura al compositor: el último fotograma
  gana, por triple búfer, con cada descarte contado (plan 020, hecho; ADR 0006).
- Pantalla, ventana, cámara y audio del sistema en la plataforma prioritaria.
- Corrección de la deriva entre relojes de dispositivos, con remuestreo de razón
  variable (venida de la fase 3).
- El vídeo capturado anclado al reloj de audio (venido de la fase 4).
- **Criterio de salida:** captura a 60 fps con frames perdidos medidos y
  acotados.

## Fase 6 — Streaming
- Fotogramas B: el muxer ordenando por tiempo de decodificación (venidos de la
  fase 4).
- Transporte en vivo (RTMP/SRT/WHIP), gestión de red, reconexión y bitrate
  adaptativo.

## Fase 7 — Interfaz
- Previsualización, editor de escenas, mezclador, ajustes, estadísticas en vivo.

## Fase 8 — Extensiones
- Registro de plugins con aislamiento, para que una extensión defectuosa no
  pueda tumbar el estudio.
