# 015 — Sincronía A/V

- **Fase:** 3 (la cierra)
- **Estado:** en curso

## Objetivo

El reloj maestro y la política que ancla el vídeo a él. `AudioClock` cuenta
muestras y las convierte en tiempo exacto; `AvSync` decide, para cada frame de
vídeo, si toca presentarlo, esperar, o descartarlo por llegar tarde. Con desfase
manual por fuente y contadores de lo que pasa.

CLAUDE.md §5 lo fija: **el audio manda y el vídeo se ajusta**. Este paso es
donde eso deja de ser una frase y pasa a ser código con tests.

## Fuera de alcance

- **El bucle del motor.** Quién llama a esto, con qué colas y en qué hilo es
  `voltra-engine`, que no existe todavía. Aquí está la *política*, pura y
  comprobable; el mecanismo llega con la fase de captura, que es la primera que
  tiene relojes de hardware de verdad que desincronizar.
- **Colas circulares y el búfer de ~1 s** con el que OBS alinea fuentes de
  relojes distintos. Es almacenamiento, no decisión, y su tamaño se decide
  midiendo con dispositivos reales.
- **Corrección de deriva por remuestreo variable.** Cuando dos relojes de
  hardware van a ritmos distintos de verdad, la respuesta correcta es un
  remuestreador de razón variable en un lazo de control. El plan 014 lo dejó
  fuera a propósito y sigue fuera: aquí se *mide* la deriva y se reporta.
- **Frames de vídeo.** Esto trabaja sobre marcas de tiempo, no sobre píxeles.
  Es mejor diseño y además mantiene `voltra-audio` sin depender de `voltra-render`.

## Referencia

`docs/references/obs-studio.md` §3, ya escrito.

De ahí salen tres cosas y las tres se adoptan:

1. **Alineación por marca de tiempo, no por orden de llegada.** Un frame vale por
   cuándo dice que es, no por cuándo apareció.
2. **Aislamiento del fallo por fuente.** Una fuente que se sale del margen sufre
   un *dropout* sin arrastrar a las demás (OBS lo arregló así en su PR #3863).
3. **Desfase manual por fuente, con valores negativos.** El `Sync Offset (ms)`
   de OBS. Existe porque ningún cálculo automático acierta con una mesa de
   mezclas externa o un micro por Bluetooth, y el usuario sí sabe lo que ve.

**Qué mejoramos:**

1. **El reloj es exacto por construcción.** `AudioClock` cuenta muestras enteras
   y convierte con la aritmética de 128 bits del plan 011. No hay acumulación de
   nanosegundos redondeados que derive con las horas.
2. **La decisión es una función pura.** `classify` no toca colas, no muta nada y
   no sabe qué es un frame: toma dos marcas de tiempo y devuelve qué hacer. Eso
   la hace exhaustivamente comprobable, que es raro en código de sincronía.
3. **La deriva se mide, no solo se corrige.** Cuánto se ha esperado, cuánto se ha
   descartado y a qué distancia va el vídeo son contadores permanentes
   (CLAUDE.md §4.10).

## Diseño

En `voltra-audio`, que es donde CLAUDE.md §2 pone la sincronía A/V.

### `AudioClock`

```rust
pub struct AudioClock { rate: SampleRate, frames: u64 }

impl AudioClock {
    pub fn advance(&mut self, frames: usize) -> Timestamp;  // devuelve el nuevo ahora
    pub fn now(&self) -> Timestamp;
    pub fn frames(&self) -> u64;
}
```

El contador de muestras es la verdad; el tiempo se deriva. Es la misma regla que
`Fps::pts` en vídeo y por el mismo motivo: sumar duraciones deriva, dividir un
índice absoluto no.

### `SyncOffset`

Con signo, en nanosegundos, acotado a ±5 s. Positivo significa **el vídeo se
retrasa** respecto al audio; negativo, se adelanta. El sentido se documenta y se
fija con un test, porque es exactamente el tipo de cosa que se implementa al
revés y nadie lo nota hasta que un usuario se queja de que el control va del
lado contrario.

### `AvSync`

```rust
pub enum FrameFate { Early, OnTime, Late, Dropout }

pub struct AvSync { offset: SyncOffset, tolerance: Duration, window: Duration }

impl AvSync {
    /// Dónde debería estar el vídeo para este instante de audio.
    pub fn deadline(&self, audio: Timestamp) -> Timestamp;
    /// Qué hacer con un frame que dice ser de `pts`.
    pub fn classify(&self, audio: Timestamp, pts: Timestamp) -> FrameFate;
}
```

- `Early`: el frame es del futuro más allá de la tolerancia. Se espera; el
  anterior se mantiene en pantalla.
- `OnTime`: dentro de la tolerancia. Se presenta.
- `Late`: pasado, pero dentro de la ventana. Se descarta y se sigue buscando el
  bueno — que es lo que evita que un tirón acumule retraso para siempre.
- `Dropout`: fuera de la ventana. La fuente ha perdido el hilo; se reporta y **no
  se bloquea el resto**.

La tolerancia por omisión es **medio frame** a la cadencia en uso, que es la
mayor distancia a la que un frame sigue siendo el correcto. La ventana por
omisión es 1 s, el mismo orden que OBS.

### Contadores

`SyncStats { presented, held, dropped, dropouts, drift }`, con `drift` en
nanosegundos con signo: lo lejos que iba el vídeo la última vez que se decidió.

## Pasos

1. `AudioClock` y sus tests de exactitud y monotonía.
2. `SyncOffset` con su signo fijado por test.
3. `AvSync::classify` y las cuatro fronteras.
4. `SyncStats` y el seguimiento de la deriva.
5. Test de integración: una secuencia larga simulada.
6. Documentación, puerta de calidad, y **cerrar la fase 3**.

## Verificación

- **El reloj no deriva.** A 48 kHz, la muestra 48 000 es exactamente 1 s; a las
  diez horas sigue siendo exacto. Ya está probado para `SampleRate`; aquí se
  prueba que contar bloques da lo mismo que contar muestras.
- **El reloj es monótono.** Nunca retrocede, ni siquiera al avanzar cero.
- **El signo del desfase.** Un desfase positivo de 100 ms hace que un frame que
  antes llegaba a tiempo llegue pronto, no tarde. Es el test que impide
  implementarlo al revés.
- **Las cuatro fronteras de `classify`**, cada una comprobada justo a un lado y
  justo al otro: tolerancia arriba, tolerancia abajo, ventana.
- **Un frame exactamente en el límite** tiene un lado asignado y documentado, no
  el que salga.
- **Integración: un minuto de 29,97 fps contra un reloj de 48 kHz.** Se cuenta
  cuántos frames se presentan y cuántos se mantienen, y se compara con lo que
  dice la aritmética. Si la política estuviera sesgada, el conteo se iría.
- **Una fuente que se detiene** produce `Dropout` y no bloquea: el reloj sigue.
- **Una fuente adelantada de golpe** —un salto de marca de tiempo— se resuelve
  descartando, no acumulando retraso.

Criterio de aceptación: la simulación de un minuto presenta exactamente los
frames que la cadencia implica, ±1, y ninguna frontera queda sin test.

## Riesgos

- **El sentido del desfase es una moneda al aire hasta que se fija.** Mitigado
  con un test que lo expresa en términos de comportamiento observable, no de
  signo aritmético.
- **La tolerancia de medio frame puede oscilar** cuando la cadencia de vídeo y
  la del audio son casi coprimas: un frame podría alternar entre presentado y
  mantenido. La simulación larga es justo lo que lo detectaría, contando.
