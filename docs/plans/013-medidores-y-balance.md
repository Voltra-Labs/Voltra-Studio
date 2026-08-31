# 013 — Medidores y balance: la banda de canal

- **Fase:** 3
- **Estado:** completado (2026-08-31) — ver Resultado

## Objetivo

Completar la banda de canal del mezclador: cada pista gana **balance** y el
mezclador publica **niveles** —pico y RMS, por canal, por pista y de la mezcla—
que la interfaz puede leer desde otro hilo sin bloquear ni al audio ni a sí
misma. Todo dentro de las mismas reglas del plan 012: cero asignaciones, cero
bloqueos.

## Fuera de alcance

- **Balística de medidor** (caída, retención de pico, suavizado). libobs no la
  tiene en el núcleo y nosotros tampoco: es política de presentación y vive con
  la interfaz, en la fase 7. Aquí se publican números crudos por bloque.
- **True peak.** El medidor de OBS puede sobremuestrear ×5 para encontrar picos
  entre muestras. Es cuatro veces el trabajo y solo importa al vigilar un
  limitador de emisión; llega con su medición cuando haya un limitador.
- **Curvas de fader** (cúbica, IEC, logarítmica). Son mapeos de interfaz; el
  mezclador recibe ganancia lineal y así se queda.
- **Paneo de fuentes mono.** Distribuir una señal mono por el campo estéreo es
  una operación distinta del balance, con otra ley (potencia constante). Llega
  cuando exista una fuente mono que panear.
- **Remuestreo y sincronía A/V**, que son los dos planes que cierran la fase.

## Referencia

`docs/references/audio.md` §5, escrito para este plan.

**Qué adoptamos:**

1. **Pico y RMS por bloque y por canal**, que es exactamente lo que calcula
   `obs_volmeter`.
2. **La balística fuera del núcleo.** El hallazgo de la referencia: en libobs no
   hay constantes de caída ni retención, y el campo `update_ms` ni se usa. Todo
   eso está en su interfaz. Es la separación correcta.

**Qué mejoramos:**

1. **El balance tiene ley documentada.** La API de OBS es un `float` sin rango
   ni ley en sus cabeceras públicas. Aquí es un tipo con rango declarado, una
   ley elegida y el porqué escrito.
2. **Los niveles se publican sin desgarro dentro de un canal.** Pico y RMS de un
   mismo canal viajan en un único `AtomicU64`, así que la interfaz nunca lee un
   pico de un bloque con la RMS de otro. Entre canales sí puede haber desfase de
   un bloque, y eso se documenta como aceptable: son 21 ms en algo que se mira.

## Diseño

### `Balance`

Rango **−1,0 (todo a la izquierda) … +1,0 (todo a la derecha)**, 0,0 al centro.
Declarado, a diferencia del referente.

La ley es **balance sin realce**: al girar a la derecha se atenúa el canal
izquierdo y el derecho se queda en su ganancia; nunca se sube nada.

```
izquierda = min(1, 1 − balance)      derecha = min(1, 1 + balance)
```

Por qué esta y no potencia constante: la ley de potencia constante (−3 dB al
centro) existe para **panear una fuente mono** por el campo estéreo, donde subir
un lado mientras baja el otro mantiene la sonoridad percibida. Aplicada al
**balance de una fuente que ya es estéreo** tendría que realzar un canal por
encima de su nivel original, que es una forma silenciosa de saturar al mover un
control que el usuario cree que solo atenúa. Un control que solo atenúa no puede
hacer clipping.

### Niveles

```rust
pub struct ChannelLevel { pub peak: f32, pub rms: f32 }   // amplitud lineal
pub struct Levels { … }                                    // hasta MAX_CHANNELS
```

Publicados en un `[AtomicU64; MAX_CHANNELS]` por pista y otro para la mezcla:
los dos `f32` de un canal empaquetados en una palabra, escritos con un `store`
por canal y por bloque.

Se miden **después** de ganancia y balance, que es lo que el usuario espera del
medidor de una pista: lo que esa pista aporta a la mezcla, no lo que entró.

En amplitud lineal, no en decibelios: convertir es un logaritmo por canal y por
bloque en el hilo de audio, y quien lo muestra ya tiene que convertir a píxeles
de todos modos. `Gain::db` ya existe para eso.

### El coste

Medir añade a cada muestra un valor absoluto, un máximo y una
multiplicación-acumulación. CLAUDE.md §4.10 dice que las métricas internas van
**siempre encendidas**, así que no hay interruptor: se mide el coste, se publica,
y si algún día molesta se decide con el número delante.

## Pasos

1. `Balance` con su ley y sus tests.
2. `ChannelLevel`, `Levels` y su publicación atómica.
3. Integrar ambos en el bucle de mezcla.
4. Ampliar el test de asignación cero a los caminos nuevos.
5. Medir el coste del medidor contra el plan 012.
6. Documentación, puerta de calidad y commit.

## Verificación

- **El balance solo atenúa.** Ninguna posición sube ningún canal por encima de
  su ganancia. Es el test que fija la ley elegida.
- Centro deja los dos canales intactos; extremo izquierdo silencia el derecho
  por completo, y al revés.
- El balance **se compone** con la ganancia en vez de reemplazarla.
- El balance también se interpola: moverlo de golpe no puede producir el clic
  que el plan 012 eliminó de la ganancia.
- **El pico es el pico**: una señal que llega a 0,8 reporta 0,8; una onda
  cuadrada de amplitud 1 reporta RMS 1; una senoide de amplitud 1 reporta RMS
  ≈ 0,707 (1/√2), que es la comprobación que separa una RMS de verdad de una
  media de valores absolutos.
- Silencio reporta cero, no un residuo.
- Los niveles de la mezcla son los de la suma, no la suma de los niveles.
- Los niveles se leen desde otro hilo mientras se mezcla, sin bloqueos.
- **Cero asignaciones** con medición y balance activos, comprobado con el
  asignador contador del plan 012.

Criterio de aceptación: los tests anteriores pasan y el coste añadido por el
medidor está medido contra la cifra del plan 012 (4,16 µs con 8 pistas).

## Riesgos

- **El medidor duplica el coste de la mezcla.** Es plausible: son tres
  operaciones más por muestra sobre un bucle que hacía dos. Aun duplicándose
  serían 8 µs sobre un bloque de 21,3 ms, el 0,04 %. Si sale peor de lo
  esperado, se anota y se decide entonces, no antes.
- **La RMS por bloque es una RMS de 21 ms**, no la de una ventana perceptual.
  Para un medidor de mezcla es lo correcto y es lo que hace OBS; para medir
  sonoridad de programa hace falta otra cosa (BS.1770), y eso no es esto.

## Resultado

Puerta de calidad en verde: **242 tests** (desde 216), `clippy -D warnings` y
`fmt` limpios. `voltra-audio` pasa de 24 a 46 tests unitarios, de 6 a 7 de
asignación, y de 3 a 4 doctests.

### Lo que quedó hecho

- `Balance` con rango declarado y ley documentada, y `TrackHandle::set_balance`.
- `ChannelLevel`, `Levels` y publicación atómica por pista y de la mezcla.
- Gain y balance **plegados en una sola ganancia por canal**, así que la rampa
  del plan 012 cubre los dos sin mecanismo nuevo: mover el balance de golpe
  tampoco hace clic, y hay test para ello.
- El test de asignación cero cubre además medición y balance, con lectura de
  medidores incluida.

### El coste, y la estimación que estaba mal

El plan estimó que medir duplicaría el coste. **Lo multiplicó por 6,3**: 8
pistas pasan de 4,16 µs a 25,9 µs. La estimación no contaba con que el bucle
original vectoriza y el medidor lo impide.

| Caso, 1024 muestras estéreo | Plan 012 | Plan 013 |
|---|---|---|
| 1 pista, asentado | 1,31 µs | 5,80 µs |
| **8 pistas, asentado** | **4,16 µs** | **25,9 µs** |
| 16 pistas, asentado | 7,78 µs | 48,3 µs |
| 8 pistas, en rampa | 14,7 µs | 29,3 µs |

En absoluto sigue siendo pequeño: 25,9 µs es el **0,12 %** de un bloque de
21,3 ms. Se queda encendido, porque CLAUDE.md §4.10 pide las métricas siempre
encendidas y un medidor que hay que activar es un medidor apagado cuando hace
falta.

### Dos intentos de arreglarlo, los dos peores

Detalle completo en `docs/PERFORMANCE.md` §3.8.

1. **Pico sin ramas** (`f32::max` en vez de `if`): **1,8× peor**, 26,3 → 46,5 µs.
   `f32::max` lleva la semántica NaN de IEEE 754 y no compila a una sola
   instrucción. La lección del plan 008 —el `clamp` sin ramas, −28 %— **no se
   transfiere**: allí se quitaba una cadena de `if/else if/else`, aquí se metía
   una función con casos especiales.
2. **`mul_add` para la suma de cuadrados**: **3× peor**, 26,3 → 78,2 µs. Es la
   segunda vez que este proyecto cae en `mul_add`, dos planes después de
   documentarlo en §3.7. Queda anotado en el código además de en el registro.

Los dos están escritos porque un experimento fallido sin documentar es tiempo
que alguien volverá a perder — y en este caso ya lo perdí yo dos veces.

### Lo demás que se verificó

La ley del balance fija con un test que recorre las 201 posiciones y exige que
**ninguna suba ningún canal**: si alguien cambia a potencia constante, falla. La
RMS se comprueba contra una senoide, donde tiene que dar 1/√2 ≈ 0,707 y no 2/π
≈ 0,637, que es lo que daría una media de valores absolutos. Los niveles de la
mezcla son los de la suma y no la suma de los niveles: dos pistas a 0,5 que
coinciden dan 1,0 y saturan, y las mismas dos en oposición dan 0.

## Desviaciones respecto al plan

- **La estimación de coste del plan estaba mal por 3×.** Está arriba con el
  número real.
- **`Balance::gain_for` deja en paz los canales más allá del estéreo.** No
  estaba previsto: un control de balance habla del campo estéreo, y atenuar en
  silencio un canal de surround con él sería una sorpresa.
- **`Levels::silent` y `publish_silence`** aparecieron al descubrir que una
  pista sin entrada seguía mostrando el último bloque que sonó. Tiene test.
