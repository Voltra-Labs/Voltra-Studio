# 012 — Mezclador multipista

- **Fase:** 3
- **Estado:** en curso

## Objetivo

`Mixer`: suma N pistas en un búfer de salida, con ganancia por pista, silencio,
control desde otro hilo y ganancia interpolada para que mover un fader no
produzca clics. **Sin una sola asignación ni un solo bloqueo en la llamada de
mezcla**, y demostrado con un test, no afirmado.

Es el **criterio de salida de la fase 3**.

## Fuera de alcance

- **Paneo y balance.** Necesitan elegir una ley (balance lineal frente a
  potencia constante) y esa es una decisión documentada con su propio apartado.
  Va en el plan 013, junto con los medidores.
- **Medidores** (pico con balística, RMS, true peak). Aquí solo se registra el
  pico del bloque como contador, que es lo mínimo para saber si se está
  saturando (CLAUDE.md §4.10), no un medidor.
- **Alineación por marca de tiempo y colas circulares.** El mezclador recibe
  bloques ya alineados. Alinear fuentes con relojes distintos —lo que OBS
  resuelve con ~1 s de buffering— es un plan propio.
- **Remuestreo.** Todas las pistas llegan a la misma frecuencia; una que no lo
  esté se rechaza en vez de sonar mal.
- **Mezclas múltiples.** libobs tiene seis buses independientes. Aquí hay uno;
  el segundo llegará cuando exista un destino que lo pida.

## Referencia

`docs/references/audio.md` §4, escrito para este plan a partir de
`mix_audio()` en `libobs/obs-audio.c`.

**Qué adoptamos:**

1. **Sumar es `+=`, sin normalizar por número de pistas.** Normalizar haría que
   subir una pista bajase las demás.
2. **Sin recorte en la mezcla.** La suma vive en float y puede pasar de 1,0. El
   recorte ocurre una sola vez, al convertir a formato de salida — que es lo que
   ya hace la conversión del plan 011 al saturar.
3. **Trabajo por bloques de tamaño fijo.**

**Qué mejoramos:**

1. **Ganancia interpolada.** Un cambio de ganancia entre bloques es una
   discontinuidad en la onda, y una discontinuidad es energía en todas las
   frecuencias: un clic. A 48 kHz con bloques de 1024 muestras, mover un fader
   produciría un clic cada 21 ms. Interpolar a lo largo del bloque cuesta un
   multiplicar-acumular por muestra en vez de un multiplicar, y lo elimina.
2. **El control es sin bloqueos por construcción, no por disciplina.** La
   ganancia y el silencio viven en atómicos que el hilo de audio lee una vez por
   bloque. No hay `Mutex` en el tipo, así que no hay forma de añadir uno sin que
   se vea en la revisión.
3. **La ausencia de asignaciones se demuestra.** Un asignador global que cuenta
   envuelve la llamada de mezcla en un test. El criterio de salida de la fase
   deja de ser una afirmación en un documento.

## Diseño

Crate nueva `voltra-audio`, que depende solo de `voltra-core`.

```rust
pub struct Gain(f32);                 // amplitud lineal, con conversión a dB
pub struct TrackId(u32);
pub struct TrackHandle(Arc<TrackControl>);   // lo que sostiene la interfaz

struct TrackControl {                 // compartido entre hilos, sin bloqueos
    gain_bits: AtomicU32,             // los bits de un f32
    muted: AtomicBool,
}

pub struct Mixer {
    rate: SampleRate,
    layout: ChannelLayout,
    tracks: Vec<Track>,               // preasignado, nunca crece al mezclar
    stats: MixStats,
}

pub struct MixStats { mixed, skipped, peak, ramping }
```

`add_track` y `remove_track` son **plano de control**: asignan, y se llaman
desde el hilo de interfaz. `mix` es **plano de datos**: no asigna nada.

La interpolación, por pista y por bloque:

1. Leer el objetivo de los atómicos, con `Ordering::Relaxed`. Una lectura por
   pista por bloque, no por muestra: a 48 kHz eso es 47 lecturas por segundo.
   `Relaxed` basta porque no hay nada que ordenar respecto a ello — un fader que
   llega un bloque tarde es indistinguible de uno movido 21 ms después.
2. Si el objetivo es igual a la ganancia actual, camino rápido: un escalar por
   muestra.
3. Si no, rampa lineal desde la actual hasta el objetivo a lo largo de
   `RAMP` milisegundos, limitada para no pasarse.

Suma: `salida[i] += entrada[i] * ganancia`. Sin recorte, como el referente.

## Pasos

1. Crate `voltra-audio` con `Gain` y su conversión a decibelios.
2. `TrackControl`, `TrackHandle` y los identificadores.
3. `Mixer::mix`: suma, camino rápido y rampa.
4. Test de asignación cero con asignador contador.
5. `benches/mix.rs`.
6. Documentación, puerta de calidad y commit.

## Verificación

El criterio de salida de la fase, primero:

- **Cero asignaciones en `mix`.** Un asignador global contador envuelve la
  llamada; el contador tiene que quedarse exactamente en cero, con pistas
  añadidas, quitadas, silenciadas y en plena rampa.
- **Cero bloqueos**, por construcción: no hay `Mutex` ni `RwLock` en la crate, y
  el control es atómico. Se comprueba además mezclando desde un hilo mientras
  otro mueve los faders.

Lo demás:

- Sumar dos señales opuestas da silencio exacto.
- La suma **no** se recorta: dos pistas a 0,8 dan 1,6, no 1,0.
- Ganancia de silencio deja la salida intacta; ganancia unitaria copia.
- Una pista silenciada no aporta nada y se cuenta como saltada.
- **Un salto de ganancia no produce discontinuidad**: la diferencia entre
  muestras consecutivas durante la rampa está acotada, y sin rampa no lo estaría.
  Es el test que justifica la mejora nº 1.
- La rampa **llega** al objetivo y no lo pasa, subiendo y bajando.
- `0 dB` es ganancia 1,0; `-6 dB` es ~0,501; `-inf dB` es 0.
- Una pista con frecuencia o disposición distinta a la del mezclador se rechaza.
- Un identificador desconocido se ignora y se cuenta, no rompe la mezcla
  (CLAUDE.md §5: un fallo de una fuente no tumba el resto).
- El pico reportado es el de la salida, incluido cuando pasa de 1,0.

Criterio de aceptación: **el test de asignación cero pasa**, y el coste de
mezclar 8 pistas de 1024 muestras estéreo está medido y anotado.

## Riesgos

- **El asignador contador mide el test, no solo la mezcla.** Un `Vec` construido
  por el propio test contaría. Mitigación: el contador se activa con un atómico
  justo antes de la llamada y se apaga justo después, y hay un test negativo que
  comprueba que el contador **sí** detecta una asignación deliberada — si no,
  estaría midiendo nada y pasaría siempre.
- **La rampa oculta un error de ganancia.** Si la rampa nunca terminase, la
  ganancia estacionaria sería incorrecta para siempre y los tests de suma
  seguirían pasando por poco. Mitigación: un test comprueba la convergencia
  exacta al objetivo.
