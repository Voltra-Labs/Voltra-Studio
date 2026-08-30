# 009 — Salida Y4M

- **Fase:** 2
- **Estado:** completado (2026-08-30) — ver Resultado

## Objetivo

Que `voltra render` produzca un fichero de vídeo reproducible. Compone una
escena animada durante N frames, convierte el lienzo BGRA a I420 y lo escribe
como YUV4MPEG2. **Es el primer hito visible del proyecto**: a partir de aquí
cada paso se puede *ver* con `ffplay`, no solo testear.

## Fuera de alcance

- **Codificación real.** Y4M es vídeo crudo. H.264 y AV1 son la fase 4, detrás
  del ADR 0003. Quien quiera un MP4 hoy puede encadenar:
  `voltra render -o - | x264 --demuxer y4m - -o salida.mp4`.
- **Un trait `Output`.** Hay una sola implementación; el razonamiento está en
  `docs/references/y4m.md` §3. Llegará con el muxer a fichero de la fase 4.
- **Audio.** Y4M es solo vídeo, y el mezclador es la fase 3.
- **Formatos Y4M que no necesitamos.** 4:2:2, 4:4:4, 10 bits, entrelazado y
  `444alpha` quedan fuera. Se escriben `C420jpeg` y `Cmono`, que es lo que el
  pipeline produce hoy.
- **Lectura de escenas desde fichero.** La escena de demostración va en código.
  Un formato de proyecto es una decisión de arquitectura con su propio ADR.
- **Optimizar el camino CPU.** El plan 008 ya dio el veredicto: el compositor
  está 8× por encima del presupuesto y la solución es la GPU, no afinar más
  (`docs/PERFORMANCE.md` §3.4). Aquí se **mide** el coste total por frame; no se
  intenta arreglarlo.

## Referencia

`docs/references/y4m.md`, escrito para este plan. En resumen:

- La especificación `yuv4mpeg(5)` de mjpegtools define la cabecera de texto, los
  tags `W H F I A C X` y el marcador `FRAME`.
- FFmpeg (`yuv4mpegenc.c`) fija el orden de tags que todos los lectores
  aceptan y añade dos convenciones sobre el tag libre `X`: `XYSCSS=` y
  `XCOLORRANGE=`.

**Qué adoptamos:** el orden de tags de FFmpeg y sus dos convenciones.

**Qué mejoramos:**

1. **El código de croma se deriva del conversor.** El plan 004 promedia el
   bloque 2×2 antes de convertir, lo que sitúa el croma en el centro del
   bloque: eso es `420jpeg`, y eso se escribe. Escribir `420mpeg2` "porque es lo
   normal" desplazaría el color medio píxel.
2. **El rango de color nunca falta**, porque lo conocemos: sale del `ColorSpec`
   con el que se convirtió.
3. **Cero asignaciones por frame** en el escritor (CLAUDE.md §4.1).
4. **El stride nunca llega al fichero.** Se emiten `row_bytes` por fila, jamás
   `stride`: nuestros planos van alineados a 32 bytes y con relleno entre
   ellos.

## Diseño

Crate nueva `voltra-output`, que depende solo de `voltra-core`. Es la posición
que le da CLAUDE.md §2 —muxers y transporte— y respeta la dirección de
dependencias: no conoce ni a `voltra-render` ni a `voltra-engine`.

```rust
/// Lo que va en la cabecera y no se deduce del frame.
pub struct Y4mParams {
    pub fps: Fps,
    pub interlacing: Y4mInterlacing,   // Progressive por defecto
    pub pixel_aspect: (u32, u32),      // 1:1 por defecto
    pub range: ColorRange,             // Limited por defecto
}

pub struct Y4mWriter<W: Write> { /* … */ }

impl<W: Write> Y4mWriter<W> {
    pub fn new(writer: W, format: PixelFormat, size: FrameSize, params: Y4mParams) -> Result<Self>;
    pub fn write_frame(&mut self, frame: &VideoFrame) -> Result<()>;
    pub fn frames_written(&self) -> u64;
    pub fn finish(self) -> Result<W>;
}
```

Invariantes:

- La cabecera se escribe **una vez**, en `new`. A partir de ahí el formato y el
  tamaño están fijados y `write_frame` rechaza cualquier frame que no case:
  cambiar de resolución a mitad de un Y4M produce un fichero que ningún lector
  entiende, así que se falla en vez de escribirlo.
- Formatos aceptados: `I420` → `C420jpeg`, `Y8` → `Cmono`. `Nv12` es
  semiplanar y Y4M pide planos separados: `Error::Unsupported` con el motivo,
  no una conversión silenciosa. `Bgra8`/`Rgba8` tampoco: convertir es trabajo
  del llamante, y hacerlo aquí escondería un coste de 4 ms por frame dentro de
  lo que parece una escritura.
- Escritura por plano: un `write_all` cuando `stride == row_bytes`, y un bucle
  por filas cuando no. La función que lo hace toma `(bytes, stride, row_bytes,
  height)` sueltos, para que el camino con relleno se pueda testear hoy aunque
  todavía no exista ninguna fuente que produzca frames con relleno.

En el CLI, `voltra render`:

```
voltra render -o <PATH|->  [--size 1280x720] [--fps 60] [--frames 120]
                           [--background RRGGBB] [--filter point|bilinear]
```

El bucle por frame, en el orden que fija `docs/references/obs-studio.md` §2 —el
mismo que adoptó el plan 008—:

1. `tick` de todas las fuentes.
2. Animar los transforms de la escena para este frame.
3. `CpuCompositor::composite` → lienzo BGRA.
4. `rgb_to_yuv` → frame I420 reutilizado, nunca reasignado.
5. `Y4mWriter::write_frame`.

La escena de demostración se construye con `ColorSource`, la única fuente que
existe: un fondo y cuatro barras que orbitan el centro rotando, escalando y con
alfa distinto, para que el fichero resultante ejercite de un vistazo lo que el
plan 008 dejó hecho —colocación, rotación, escalado, recorte contra el borde y
mezcla alfa—. Es determinista: el frame *n* depende solo de *n*.

Los tres tiempos —componer, convertir, escribir— se miden por frame y se
resumen por stderr al terminar (CLAUDE.md §4.10). stdout queda libre para el
vídeo, que es lo que hace posible `voltra render -o - | ffplay -`.

## Pasos

1. Crate `voltra-output` con `Y4mWriter`, su cabecera y sus tests.
2. `voltra render` en el CLI: escena de demostración, bucle y métricas.
3. Test de integración de ida y vuelta: renderizar, releer, comparar.
4. Actualizar `docs/PERFORMANCE.md`, `docs/ROADMAP.md` y CLAUDE.md §8.
5. Puerta de calidad y commit.

## Verificación

Sobre el escritor:

- La cabecera de 1280×720 a 60 fps es **byte a byte**
  `YUV4MPEG2 W1280 H720 F60:1 Ip A1:1 C420jpeg XYSCSS=420JPEG XCOLORRANGE=LIMITED\n`.
- 29,97 fps se escribe `F30000:1001`, no `F29:1` ni un decimal: es la razón por
  la que el reloj del plan 002 es racional.
- Rango completo cambia el tag a `XCOLORRANGE=FULL`; `Y8` cambia el croma a
  `Cmono` y escribe solo el plano de luma.
- Cada frame añade exactamente `6 + anchura × altura × 3 / 2` bytes en 4:2:0.
- Un frame de tamaño o formato distinto al de la cabecera es rechazado y **no**
  escribe nada.
- `Nv12` y `Bgra8` se rechazan en la construcción.
- Un plano con relleno emite `row_bytes` por fila: el relleno no aparece en la
  salida.
- Un `Write` que falla propaga el error en vez de entrar en pánico.

Sobre el conjunto:

- Ida y vuelta: componer un lienzo conocido, escribirlo, releer el fichero
  entero y comprobar que los planos coinciden con lo que produjo `rgb_to_yuv`.
- `voltra render --frames 3` a un fichero temporal produce el tamaño exacto
  esperado y una cabecera válida.
- La escena de demostración es determinista: dos ejecuciones dan ficheros
  idénticos.

Criterio de aceptación: **el fichero se abre y se ve** en un reproductor
externo, y el test de ida y vuelta pasa en CI sin reproductor. El coste por
frame se mide y se anota; no hay umbral que cumplir, porque el presupuesto ya lo
rompió el compositor y eso está documentado en `docs/PERFORMANCE.md` §3.4.

## Riesgos

- **El fichero se abre pero se ve mal** (color desplazado, verde, lavado). Es el
  fallo típico de una primera salida y viene siempre de la cabecera: croma,
  rango o stride. Mitigación: los tres van fijados por tests de cabecera exacta,
  y el de ida y vuelta descarta el stride. Si aun así falla, el sospechoso es el
  conversor del plan 004, no el escritor.
- **El coste de escribir domina y ensucia la medición.** 187 MB/s a 1080p60
  puede saturar el disco del contenedor. Mitigación: el tiempo de escritura se
  mide aparte de los otros dos, así que se ve en vez de contaminar.
- **Tentación de meter escalado de salida aquí.** OBS reescala antes de
  convertir y el plan 008 lo señaló como el arreglo barato de la deuda §3.1.
  Es un paso propio, con su medición: se anota, no se cuela.

## Resultado

Puerta de calidad en verde: **155 tests** (87 en `voltra-core`, 15 en
`voltra-sources`, 14 imágenes doradas, 12 + 2 en `voltra-output`, 9 + 7 en
`voltra-cli`, 9 doctests). Sube desde los 124 del plan 008.

**El criterio de aceptación se cumple: el fichero se ve.** Se decodificó la
salida con un lector independiente escrito para la ocasión y se convirtió a PNG:
las cuatro barras orbitan, giran, laten, se recortan contra los cuatro bordes y
el panel translúcido se ve a través de las dos barras con alfa. Los colores
salen donde deben, que es la comprobación de que el conversor del plan 004 y el
orden de planos son correctos.

### Lo que quedó hecho

- Crate `voltra-output` con `Y4mWriter`, `Y4mParams` y `Y4mInterlacing`.
  `I420` → `C420jpeg`, `Y8` → `Cmono`; el resto se rechaza en la construcción.
- `voltra render` con `--size`, `--fps`, `--frames`, `--background`, `--filter`,
  `--full-range` y `-o -` para tubería.
- Escena de demostración animada, determinista y construida solo con
  `ColorSource`.
- Métricas por etapa —componer, convertir, escribir— por stderr.

### Cifras, perfil `release`, 60 frames

| Caso | Componer | Convertir | Escribir | Total |
|---|---|---|---|---|
| 720p, bilineal, a fichero | 8,00 ms | 1,89 ms | 0,84 ms | **10,7 ms** |
| 1080p, bilineal, a fichero | 17,85 ms | 4,20 ms | 3,91 ms | **25,96 ms** |
| 1080p, bilineal, a `/dev/null` | 17,92 ms | 4,25 ms | 0,004 ms | 22,18 ms |
| 1080p, vecino cercano, a fichero | 8,46 ms | 4,09 ms | 0,79 ms | **13,33 ms** |

Tres lecturas que salen de ahí:

1. **Escribir Y4M a disco cuesta 3,9 ms por frame a 1080p**, el 23 % del
   presupuesto. Contra `/dev/null` son 4 µs, así que es disco, no CPU: son los
   187 MB/s que pide el formato. Y4M es para desarrollo y tuberías, no para
   grabar; la fase 4 existe por esto.
2. **La conversión cuesta 4,2 ms**, que corrobora desde el pipeline completo el
   25 % del presupuesto que midió el plan 004 con `criterion`. Dos métodos
   independientes, el mismo número.
3. **El filtro de escalado domina la composición**: 17,9 ms bilineal contra
   8,5 ms vecino cercano, 2,1×. Es la misma proporción que midió el plan 008 en
   aislamiento, ahora sobre una escena real.

Con vecino cercano, 1080p60 queda a 13,3 ms de los 16,6 disponibles: **el
pipeline entero cabe en el presupuesto**, aunque sea en el modo de menor
calidad y sin captura ni encoding compitiendo por el tiempo. Con bilineal no
cabe. Nada de esto cambia el veredicto del plan 008: la GPU sigue siendo el
requisito.

### Un fallo que encontró un test

El subscriber de `tracing` escribe en stdout por omisión. Con `-o -`, una sola
línea de log en medio del vídeo lo hace ilegible. El test de extremo a extremo
lo detectó a la primera; el arreglo es una línea (`.with_writer(std::io::stderr)`)
y es la razón por la que los tests ejecutan el binario de verdad en vez de
llamar a `run()`.

## Desviaciones respecto al plan

- **`Y4mParams` no lleva `#[non_exhaustive]`.** Ese atributo prohíbe justo la
  expresión `Y4mParams { fps, ..Default::default() }` desde fuera de la crate,
  que es la única forma cómoda de construir una estructura de ajustes. Queda
  documentado en el propio tipo.
- **Se añadió `--full-range`**, no previsto. Sale casi gratis y convierte la
  mejora nº 2 del plan —"el rango nunca falta porque lo conocemos"— en algo que
  un test puede comprobar de verdad en vez de una afirmación.
- **El aviso de BT.601 por debajo de 720 líneas** tampoco estaba previsto. Y4M
  no tiene campo de matriz y los reproductores lo deducen de la altura, así que
  convertir con BT.709 a 480p produce un fichero que se ve mal por un motivo que
  no está en ninguna parte. Ahora lo dice.
