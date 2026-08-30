# Voltra Studio — Reglas de trabajo y arquitectura

Software de composición, grabación y streaming en vivo escrito en Rust.
Objetivo: lo que hace OBS Studio, pero con **rendimiento predecible**, **sin
crasheos por plugins** y **sin fugas de memoria**, aprovechando el sistema de
tipos y el modelo de propiedad de Rust.

> **Prioridad número uno del proyecto: rendimiento.** Cuando haya conflicto
> entre elegancia y velocidad en el camino caliente (hot path), gana la
> velocidad — pero solo con una medición que lo demuestre. Ver §4.

---

## 1. Cómo trabajamos (proceso obligatorio)

Este proyecto avanza **paso a paso**. Nada de entregas gigantes.

1. **Plan antes que código.** Toda tarea no trivial empieza con un documento en
   `docs/plans/NNN-nombre-corto.md` (plantilla en §1.1). Se presenta el plan y
   se espera aprobación antes de tocar código.
2. **Un paso = un objetivo verificable.** Cada paso debe caber en un commit
   revisable (orientativo: < ~400 líneas de diff productivo) y dejar el
   repositorio **compilando y con tests en verde**.
3. **Nunca se mezclan pasos.** Si al implementar aparece trabajo extra, se
   anota en el plan como paso futuro; no se cuela en el commit actual.
4. **Al terminar un paso:** ejecutar la puerta de calidad (§6.1), hacer commit
   con mensaje convencional y reportar qué se hizo, qué se midió y qué sigue.
5. **Nada se inventa: primero el estado del arte.** Antes de diseñar un
   subsistema se documenta cómo lo resuelven los referentes —OBS Studio/libobs
   por encima de todo, y donde aplique GStreamer, FFmpeg o vMix— en
   `docs/references/`, con enlaces a la fuente. El diseño debe decir
   explícitamente **qué copiamos y qué mejoramos**. Apartarse del referente es
   legítimo, pero exige justificación escrita.
6. **Decisiones de arquitectura → ADR.** Cualquier decisión que condicione el
   futuro (backend gráfico, modelo de hilos, formato de plugins, dependencia
   pesada) se registra en `docs/adr/NNNN-titulo.md` con: contexto, opciones
   evaluadas, decisión, consecuencias. Un ADR no se edita: se supersede.

### 1.1 Plantilla de plan

```markdown
# NNN — Título
## Objetivo            (qué queda funcionando al terminar; 1-3 frases)
## Fuera de alcance    (qué NO se hace aquí)
## Referencia          (cómo lo hace OBS u otros; qué copiamos, qué mejoramos)
## Diseño              (tipos, traits, flujo de datos, invariantes)
## Pasos               (lista ordenada y atómica)
## Verificación        (tests, benchmarks, criterio de aceptación medible)
## Riesgos             (qué puede salir mal y plan B)
```

### 1.2 Definición de "hecho" (Definition of Done)

Un paso está hecho cuando **todo** esto es cierto:

- [ ] `cargo build --workspace --all-targets` sin errores.
- [ ] `cargo test --workspace` en verde.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` limpio.
- [ ] `cargo fmt --all --check` limpio.
- [ ] Los elementos públicos nuevos tienen documentación (`///`) con ejemplo si
      no es obvio.
- [ ] Si toca el hot path: hay benchmark con número antes/después.
- [ ] El plan correspondiente está actualizado con lo realmente hecho.
- [ ] Si el paso introduce diseño nuevo, la referencia usada está en
      `docs/references/` y citada en el plan.

---

## 2. Estructura del repositorio

Workspace de Cargo con crates pequeñas y de responsabilidad única. **La
dirección de las dependencias es una regla, no una sugerencia**: cada crate solo
puede depender de las que están por encima de ella en esta lista.

```
Voltra-Studio/
├── CLAUDE.md                  # este documento
├── Cargo.toml                 # workspace: members, deps y lints compartidos
├── rust-toolchain.toml        # toolchain fijado (stable + rustfmt + clippy)
├── docs/
│   ├── ROADMAP.md             # fases del producto
│   ├── adr/                   # decisiones de arquitectura
│   ├── plans/                 # planes de ejecución por paso
│   └── references/            # estado del arte (libobs, GStreamer, FFmpeg)
├── crates/
│   ├── voltra-core/           # vocabulario: frame, píxel, tiempo, geometría,
│   │                          #   traits Source/Filter/Output, errores.
│   │                          #   SIN dependencias de SO. Compila en cualquier target.
│   ├── voltra-render/         # compositor: escena -> frame. Backend GPU + fallback CPU.
│   ├── voltra-audio/          # mezclador, resampling, medidores, sincronía A/V.
│   ├── voltra-capture/        # captura por plataforma (pantalla, ventana, cámara, audio).
│   │                          #   Todo tras feature flags por SO.
│   ├── voltra-encode/         # codificación: H.264/AV1, hardware y software.
│   ├── voltra-output/         # muxers y transporte: fichero, RTMP, SRT, WHIP.
│   ├── voltra-plugin/         # registro y aislamiento de extensiones.
│   ├── voltra-engine/         # orquestador: escenas, transiciones, reloj, pipeline.
│   ├── voltra-ui/             # interfaz gráfica.
│   └── voltra-cli/            # binario headless y herramientas de desarrollo.
├── benches/                   # benchmarks de integración (criterion)
└── assets/                    # recursos de prueba (pequeños, versionados)
```

Reglas de estructura:

- **`voltra-core` no depende de nada del sistema operativo** ni de ninguna otra
  crate del workspace. Es el contrato común.
- Ninguna crate de bajo nivel conoce a `voltra-ui` ni a `voltra-engine`.
- Un módulo que pasa de ~500 líneas se divide. Un `mod.rs` solo declara y
  reexporta; la lógica vive en submódulos.
- Todo código específico de plataforma va tras `#[cfg(...)]` **y** una feature,
  y expone el mismo trait que las demás plataformas.

---

## 3. Convenciones de Rust

- **Edición 2024**, MSRV declarada en el workspace y verificada en CI.
- **Idioma:** código, comentarios de código, doc-comments y mensajes de commit
  en **inglés**; los documentos de `docs/` (planes, ADR, referencias) y la
  conversación de trabajo, en **español**.
- **`unsafe` es la excepción, no la herramienta.** Solo se permite con: (a)
  justificación escrita en un comentario `// SAFETY:` que enumere las
  invariantes, (b) encapsulado en el módulo más pequeño posible tras una API
  segura, (c) test que ejercite el camino. `unsafe_op_in_unsafe_fn` denegado.
- **Errores:** `thiserror` en librerías (enums concretos, un `Error` por crate o
  por dominio), `anyhow` solo en binarios. **Prohibido `unwrap()`/`expect()`/
  `panic!()` en código de librería** salvo invariantes imposibles documentadas;
  en tests es libre — cada crate lo habilita con
  `#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]`. Un fallo de una fuente nunca puede tumbar el pipeline.
- **API pública:** tipos concretos y `newtype` en vez de primitivos sueltos
  (`SourceId(u32)`, no `u32`). `#[non_exhaustive]` en enums que crecerán.
  Derivar `Debug` siempre; `Clone` solo si es barato o está justificado.
- **Nombres:** `snake_case` funciones/módulos, `CamelCase` tipos,
  `SCREAMING_CASE` constantes. Sin abreviaturas crípticas (`buffer`, no `bfr`).
- **Documentación:** todo elemento público lleva `///`. Cada crate lleva `//!`
  explicando su papel y sus invariantes. `#![warn(missing_docs)]` en las crates
  estables.
- **Lints compartidos** en `[workspace.lints]`: `clippy::all`,
  `clippy::pedantic` (con excepciones justificadas por módulo),
  `clippy::undocumented_unsafe_blocks`, `rust_2018_idioms`,
  `unused_qualifications`. CI corre con `-D warnings`.
- **Dependencias:** cada una se justifica en el plan o el ADR. Se prefiere el
  ecosistema mantenido y con pocas dependencias transitivas. Todo lo nativo y
  pesado (GPU, ffmpeg, pipewire, GUI) va tras feature flags: **`cargo test
  --workspace` con features por defecto debe funcionar en un contenedor headless
  sin GPU ni dispositivos**.

---

## 4. Rendimiento (la prioridad)

Presupuesto de referencia: **1080p60 = 16,6 ms por frame** para capturar,
componer, filtrar, codificar y mux. La composición no debería pasar de ~4 ms.

Reglas del hot path (bucle de render, mezcla de audio, callbacks de captura):

1. **Cero asignaciones por frame.** Los búferes se reutilizan mediante pools;
   los `Vec` se preasignan con `with_capacity`. Nada de `format!`, `to_string()`
   ni `Box::new` por frame.
2. **Cero copias evitables.** Los frames viajan por referencia contada
   (`Arc<Frame>`) o con paso de propiedad, nunca clonando píxeles. Si una copia
   es inevitable, se documenta por qué.
3. **Cero bloqueos en el hilo de render y en el callback de audio.** Sin
   mutexes, sin `malloc`, sin syscalls, sin logging con formato: se usan colas
   SPSC/lock-free y snapshots inmutables de estado.
4. **Despacho estático donde importa.** `dyn Trait` está bien en el borde
   (registro de fuentes, una llamada por frame y fuente), no dentro de un bucle
   por píxel. Los bucles por píxel son monomórficos y aptos para autovectorizar
   (`chunks_exact`, iteradores, sin indexado con `[i]`).
5. **Datos pensados para la caché.** Layout contiguo, `stride` explícito,
   alineación cuando el backend lo exige, y recorrido por filas.
6. **Paralelismo consciente.** `rayon` para trabajo por filas/tiles con umbral
   mínimo de tamaño; nunca para tareas pequeñas cuyo coste de sincronización
   supere el trabajo.
7. **La GPU es el destino natural de la composición.** El camino CPU existe como
   respaldo correcto y verificable, no como el camino principal.
8. **Medir antes y después.** Toda optimización trae número: benchmark
   `criterion` o contador del propio pipeline. Sin número, no es una
   optimización: es una opinión. Y sin perfil previo, no se optimiza nada.
9. **Perfiles de compilación:** `release` con `lto = "thin"`,
   `codegen-units = 1`, y un perfil `profiling` con símbolos de depuración.
10. **Métricas internas siempre encendidas:** tiempo de render, frames perdidos,
    frames saltados, retraso de encoder, ocupación de colas. Lo que no se mide,
    se degrada.

---

## 5. Modelo de ejecución

- Hilos con papeles claros y comunicación por canales: captura(s) → render →
  encode → output, más el hilo de UI, que **nunca** bloquea al de render.
- El estado compartido se publica como snapshot inmutable por frame; la UI edita
  una copia y la publica atómicamente.
- El reloj es rational (`num/den`), nunca `f64`, para que 29,97 fps no derive.
- El audio manda en la sincronía A/V: el vídeo se ajusta al reloj de audio.
- Cualquier componente puede fallar sin tumbar el resto: los errores se propagan
  como estado, se registran y se reintenta con backoff.

---

## 6. Calidad y verificación

- **Tests unitarios** junto al código (`#[cfg(test)] mod tests`), centrados en
  invariantes: geometría, conversiones de color, timing, mezcla.
- **Tests de imagen dorada** para el compositor: se compara contra PNG de
  referencia con tolerancia por píxel.
- **Tests de integración** en `tests/` por crate para los flujos completos.
- **Benchmarks** en `benches/` con `criterion` para todo lo que esté en el hot
  path; los resultados se anotan en el plan.
- **CI** ejecuta: fmt, clippy `-D warnings`, test, build release, y `cargo deny`
  para licencias y vulnerabilidades.

### 6.1 Puerta de calidad (ejecutar antes de cada commit)

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

---

## 7. Git

- Rama de trabajo: la que indique la tarea. Nunca se empuja a `main` directo.
- **Commits convencionales:** `feat(core): ...`, `fix(render): ...`,
  `perf(audio): ...`, `docs: ...`, `test: ...`, `chore: ...`, `refactor: ...`.
- Un commit = un paso del plan. Mensaje en inglés, cuerpo explicando el *por
  qué* cuando no sea evidente.
- No se sube nada generado (`/target`, capturas, binarios). `assets/` solo
  ficheros pequeños necesarios para tests.

---

## 8. Estado actual

**Fase 0 cerrada.** El workspace existe, la puerta de calidad corre en local y
en CI, y `voltra info` funciona. Crates vivas: `voltra-core` y `voltra-cli`; las
demás se crearán cuando haya código que meter en ellas.

**Fase 1 en curso.** `voltra-core` ya tiene reloj racional y geometría de
colocación (plan 002), con benchmarks de referencia. Siguiente: formatos de
píxel y `Frame` (plan 003), y después el grafo de escena y los traits.

Decisiones ya tomadas (ver `docs/adr/`):

| ADR | Decisión |
|---|---|
| 0001 | Render: camino CPU de referencia primero, `wgpu` como backend acelerado detrás del mismo trait. |
| 0002 | Plataforma prioritaria: Linux (PipeWire + Wayland). |
| 0003 | Encoding: hardware primero y sin copia a CPU; software solo como respaldo. |
| 0004 | Interfaz: `egui`/`eframe`. |

Referencia base del diseño: `docs/references/obs-studio.md`.
Fases del producto: `docs/ROADMAP.md`.
