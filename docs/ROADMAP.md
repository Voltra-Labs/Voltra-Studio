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

Después, por orden de utilidad: audio (fase 3), encoding real (fase 4) —que es
también lo que quita los 187 MB/s de Y4M—, captura con PipeWire (fase 5), el
momento en que el programa sirve para algo, streaming (fase 6) e interfaz
(fase 7).

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
- Codificación de vídeo y audio, muxing, grabación real reproducible.
- El vídeo anclado al reloj de audio (venido de la fase 3).
- **Criterio de salida:** grabar una escena durante minutos sin deriva A/V ni
  crecimiento de memoria.

## Fase 5 — Captura de plataforma
- Pantalla, ventana, cámara y audio del sistema en la plataforma prioritaria.
- Corrección de la deriva entre relojes de dispositivos, con remuestreo de razón
  variable (venida de la fase 3).
- **Criterio de salida:** captura a 60 fps con frames perdidos medidos y
  acotados.

## Fase 6 — Streaming
- Transporte en vivo (RTMP/SRT/WHIP), gestión de red, reconexión y bitrate
  adaptativo.

## Fase 7 — Interfaz
- Previsualización, editor de escenas, mezclador, ajustes, estadísticas en vivo.

## Fase 8 — Extensiones
- Registro de plugins con aislamiento, para que una extensión defectuosa no
  pueda tumbar el estudio.
