# Visor JSON

Visor de JSON de escritorio, ligero y rápido, escrito en Rust con [egui](https://github.com/emilk/egui).
Pensado para revisar documentos grandes, compararlos y quedarse solo con lo que importa.

## Características

### Abrir documentos
- Abre uno o varios archivos (Ctrl+O), arrástralos a la ventana o pásalos por línea de comandos.
- Pega un JSON con Ctrl+V fuera de un campo de texto y se abre en una pestaña nueva.
- Formato tolerante (JSONC): acepta comentarios `//` y `/* */`, comas finales y BOM; lee UTF-8 y UTF-16.
- Conserva el orden de las claves, las claves duplicadas y el texto original de los números (`15.000000` se ve tal cual).
- Lista de archivos recientes y recarga del archivo con F5.

### Pestañas
- Varios documentos abiertos a la vez, cada uno en su pestaña.
- Reordenables arrastrándolas.
- Nombre editable con doble clic, F2 o clic derecho → «Cambiar nombre…». Ese nombre se propone al guardar.
- Clic derecho: cambiar nombre, copiar la ruta del archivo, comparar con la pestaña activa, cerrar, cerrar las demás.
- Se cierran con la ×, con clic medio o con Ctrl+W. Un punto (•) indica cambios en la fuente sin aplicar y ⚠ un JSON con errores.

### Vistas
| Vista | Atajo | Para qué sirve |
|---|---|---|
| **Árbol** | Ctrl+1 | Plegar, expandir o compactar en una línea cualquier nodo. Modo *Auto*: compacta todo lo que quepa en un ancho dado. Botones por nivel, guías de sangría, números de línea y conteo de elementos en nodos cerrados. |
| **Tabla** | Ctrl+2 | Estilo «Inspección» de Visual Studio: Nombre, Valor y Tipo, con vista previa de objetos y arreglos. |
| **Texto** | Ctrl+3 | El JSON formateado con sangría de 2, 4 o tabulador; copiar todo o guardar. |
| **Fuente** | Ctrl+4 | Editor del texto original con validación en vivo, *Formatear* y *Minificar*. Si hay un error, indica línea y columna y lleva hasta él. |
| **Comparar** | Ctrl+5 | Diferencias entre dos pestañas (ver abajo). |

**Cuadrícula:** un arreglo de objetos se puede ver como hoja de cálculo (tecla G o clic derecho → «Ver como cuadrícula…»).

### Búsqueda
- Busca en claves y valores (Ctrl+F) y muestra cuántas coincidencias hay.
- F3 / Mayús+F3 para ir a la siguiente o la anterior; en árbol y tabla se abre el camino hasta el resultado.
- La barra de estado muestra la ruta del nodo seleccionado (`$.Encabezado.Partidas[3].Codigo`); con clic se copia.

### Filtro por claves
Muestra solo ciertas claves y el camino más corto desde la raíz hasta ellas. Por ejemplo, al filtrar por
`U_SO1_AUTORIZADO`:

```json
{ "Encabezado": { "U_SO1_AUTORIZADO": "N" } }
```

- Lista de claves en el panel «Filtro por claves»: una por fila, con el número de coincidencias de cada una.
- No distingue mayúsculas y minúsculas. Cada clave coincidente se muestra con todo su valor.
- En arreglos solo quedan los elementos que contienen alguna de las claves.
- Clic derecho en un nodo → «Filtrar por esta clave» la agrega a la lista.
- Se aplica a las vistas Árbol, Tabla, Texto y Comparar. Copiar y guardar usan el resultado filtrado.
- La lista de claves se guarda entre sesiones.

### Comparar dos JSON
Al estilo de las extensiones de comparación de VS Code:

- Lado a lado: la pestaña activa a la izquierda y la elegida en «Comparar con:» a la derecha (⇄ intercambia los lados).
- Líneas alineadas: en rojo lo que solo está a la izquierda, en verde lo de la derecha y, en las líneas modificadas, los caracteres exactos que cambiaron.
- **Ignorar orden de claves** para que `{"a":1,"b":2}` y `{"b":2,"a":1}` cuenten como iguales.
- **Solo diferencias** oculta las líneas iguales y deja contexto alrededor de cada cambio.
- **Lista de cambios** por ruta JSON (`~` modificado, `+` añadido, `−` eliminado); clic para saltar a esa línea.
- Navegación con ▲ ▼ o F7 / Mayús+F7, y una regla lateral con la posición de todas las diferencias.
- Desplazamiento horizontal sincronizado (Mayús + rueda o barra inferior).
- Si el filtro por claves está activo, solo se comparan esas claves.

### Copiar y guardar
- Menú contextual de cada nodo: copiar el valor formateado, en una línea o minificado, el texto sin comillas, la ruta o la clave.
- Guardar formateado o minificado; el nombre sugerido es el de la pestaña.

### Ajustes
Tema oscuro o claro, tamaño de letra, escala de la interfaz, sangría, cómo se pliega el árbol al abrir un
documento (Auto, por nivel, expandir todo), comas, comillas en claves, índices de arreglo y más. Los ajustes se guardan
automáticamente.

## Atajos de teclado

| Atajo | Acción |
|---|---|
| Ctrl+O | Abrir archivo(s) |
| Ctrl+V | Pegar JSON como documento nuevo |
| Ctrl+N | Documento nuevo para escribir |
| Ctrl+W / clic medio | Cerrar pestaña |
| Ctrl+Tab | Siguiente pestaña |
| F2 / doble clic en pestaña | Cambiar el nombre de la pestaña |
| F5 | Recargar archivo |
| Ctrl+1 … 5 | Árbol / Tabla / Texto / Fuente / Comparar |
| Ctrl+F, F3, Mayús+F3 | Buscar, siguiente, anterior |
| F7, Mayús+F7 | Comparar: diferencia siguiente / anterior |
| Ctrl+Enter | Aplicar cambios de la fuente |
| ↑ ↓ RePág AvPág Inicio Fin | Moverse (árbol y tabla) |
| ← → | Cerrar / expandir, ir al padre / primer hijo |
| Enter, Espacio, doble clic | Expandir / cerrar |
| C | Compactar nodo en una línea |
| H | Compactar hijos (cada hijo en una línea) |
| X | Cerrar hijos |
| E | Expandir todo el subárbol |
| A | Auto-compactar el subárbol |
| G | Ver arreglo como cuadrícula |
| Ctrl+C | Copiar el valor seleccionado |
| Ctrl / Alt / Mayús + clic en ▶ | Compactar / compactar hijos / expandir subárbol |

La lista completa está en el menú **Ayuda → Atajos de teclado**.

## Compilar y ejecutar

Requiere [Rust](https://www.rust-lang.org/tools/install) (edición 2024).

```bash
cargo run --release -- ejemplos/caso.jsonc
```

El ejecutable queda en `target/release/jsonviewer.exe`. Se le pueden pasar uno o varios archivos como argumentos;
también puede asociarse a la extensión `.json` en Windows para abrirlos con doble clic.

En Windows usa las fuentes Consolas y Segoe UI del sistema si están disponibles.
