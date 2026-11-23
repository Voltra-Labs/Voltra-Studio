# ADR 0006 — Captura: el último fotograma gana, por triple búfer

- **Estado:** aceptada (2026-11-23)
- **Fase:** 5

## Contexto

Toda fuente de captura —pantalla por PipeWire, ventana, cámara, y mañana
Windows.Graphics.Capture o ScreenCaptureKit— produce fotogramas en un hilo del
sistema y a su ritmo, y el compositor los consume en el hilo de render al ritmo
de la salida. Cómo cruzan ese borde es una decisión que heredan todos los
backends, y que toca dos reglas del proyecto: **el hilo de render no se bloquea
nunca** (CLAUDE.md §4.3) y **lo que no se mide se degrada** (§4.10).

OBS lo resuelve de dos formas (`docs/references/capture.md`):

- Fuentes asíncronas: copia a una caché y cola de hasta 30 fotogramas bajo un
  mutex recursivo, con ritmo por marca de tiempo; con la cola llena, la vacía
  entera.
- Captura de pantalla en Linux: vacía la cola de PipeWire y se queda con el
  último búfer.

Ninguna de las dos cuenta lo que se pierde.

## Opciones evaluadas

| Opción | A favor | En contra |
|---|---|---|
| **Cola con ritmo (como las fuentes asíncronas de OBS)** | Suave con una fuente que fluctúa; reproduce cada fotograma | Latencia de toda la cola; un mutex o una cola lock-free de fotogramas enteros; y para la pantalla, enseñar uno viejo cuando ya hay otro nuevo no aporta nada |
| **Canal acotado de fotogramas** (`sync_channel`) | En la biblioteca estándar | Con el canal lleno, el productor espera o pierde el **nuevo**, al revés de lo que se quiere; para recuperar el búfer hace falta un segundo canal de vuelta |
| **Triple búfer sin espera** | Ninguno de los dos hilos espera nunca; el productor escribe en su sitio; sabe si sobrescribe un fotograma no leído, así que el descarte se cuenta solo | Solo el último: sin ritmo para fuentes que lo necesiten |

## Decisión

**Triple búfer, el último gana.** Cada fuente de captura tiene uno, de
`VideoFrame`:

- El productor (el hilo del backend) escribe en su búfer en su sitio y lo
  publica. Si el anterior no se había leído, cuenta un **descarte**.
- El consumidor (el tick de la fuente, en el hilo de render) se queda con el
  último publicado, si lo hay; si no, sigue con el que tenía y cuenta una
  **repetición**.
- La implementación es la crate `triple_buffer` (8.1, MPL-2.0, con una sola
  dependencia, `crossbeam-utils`; MSRV 1.74; la 9 pide 1.86, por encima de la
  nuestra), en vez de una propia: es lo que evita escribir `unsafe` aquí, y
  lleva años con tests y benchmarks.

El ritmo con búfer para cámaras que fluctúan, si hace falta, será **otra**
entrega detrás del mismo lado consumidor, no un cambio de esta.

## Consecuencias

- La latencia de captura a compositor es como mucho un fotograma de salida.
- Cada fuente expone, sin coste, capturados, descartados, enseñados y repetidos.
  La invariante capturados = enseñados + descartados (+ el que espera) es
  comprobable.
- Tres búferes por fuente: a 1080p BGRA, 24 MB. Aceptable para una captura;
  revisable si una escena tiene decenas de fuentes vivas.
- Un cambio de tamaño reserva memoria en el hilo de captura, una vez por cambio:
  documentado, no por fotograma.
