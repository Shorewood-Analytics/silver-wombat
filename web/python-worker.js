// Runs Python off the page's main thread, using Pyodide (CPython compiled to
// WebAssembly). One worker holds one Python version. The page replaces the
// worker to change version or to stop a running program.
//
// Messages in:  JSON text {id, job}; the job is `Job` in crates/core/src/python.rs.
// Messages out: JSON text {id, progress} and, if the job asks for its output
// as it is produced, {id, output}; then {id, outcome} or {id, error}.
//
// The interpreter itself is served by this site from python/<version>/.
// Libraries (numpy and the rest) are not part of the site: Pyodide downloads
// each one from its own CDN folder the first time a program imports it.

const CDN = "https://cdn.jsdelivr.net/pyodide/";
const ROOT = "/repo";
const MAX_OUTPUT_BYTES = 1024 * 1024;
const MAX_FILE_BYTES = 2 * 1024 * 1024;
// The file name Python reports for this worker's own code, so it can be left
// out of error traces.
const HERE = "<silver-wombat>";
// Where plt.show() puts its figures, and where the code that does that lives.
const FIGURES = "/tmp/silver-wombat-figures";
const SUPPORT = "/lib/silver-wombat";

// matplotlib asks its "backend" what show() means. In a desktop terminal that
// opens a window. Here it saves each open figure as a picture, which the page
// then offers under the terminal. Everything else is matplotlib's own Agg
// backend, the one that draws to a file. It is only loaded if a program uses
// matplotlib.
const FIGURE_BACKEND = `
import os

from matplotlib._pylab_helpers import Gcf
from matplotlib.backend_bases import FigureManagerBase
from matplotlib.backends.backend_agg import FigureCanvasAgg

FOLDER = "${FIGURES}"


class FigureManager(FigureManagerBase):
    @classmethod
    def pyplot_show(cls, *, block=None):
        os.makedirs(FOLDER, exist_ok=True)
        for manager in Gcf.get_all_fig_managers():
            number = len(os.listdir(FOLDER)) + 1
            manager.canvas.figure.savefig(os.path.join(FOLDER, "figure-%d.png" % number))
        # A window that has been looked at and closed: the figure is done.
        Gcf.destroy_all()


class FigureCanvas(FigureCanvasAgg):
    manager_class = FigureManager
`;

let loading = null;
let loadedPython = null;
let chunks = [];
let outputBytes = 0;
let outputCut = false;

// When a job asks for its output as it is produced: where to send it, and
// what has been written but not sent yet.
let live = null;
let unsent = "";
let liveDecoder = null;
// Not every write is sent at once: a program printing in a loop would flood
// the page. A few pieces may go straight away, so lines printed together are
// not held back; after that, one piece every 50 ms.
const BURST = 8;
const REFILL_MS = 50;
let allowance = BURST;
let refilled = 0;

function sendOutput(force) {
  if (live === null || unsent === "") return;
  const now = Date.now();
  const earned = Math.floor((now - refilled) / REFILL_MS);
  if (earned > 0) {
    allowance = Math.min(BURST, allowance + earned);
    refilled = allowance === BURST ? now : refilled + earned * REFILL_MS;
  }
  if (allowance === 0 && !force) return;
  allowance = Math.max(0, allowance - 1);
  live(unsent);
  unsent = "";
}

function collect(buffer) {
  if (outputBytes < MAX_OUTPUT_BYTES) {
    if (live !== null) {
      unsent += liveDecoder.decode(buffer, { stream: true });
      sendOutput(false);
    } else {
      chunks.push(buffer.slice());
    }
  } else {
    outputCut = true;
  }
  outputBytes += buffer.length;
  return buffer.length;
}

function say(text) {
  collect(new TextEncoder().encode(text.endsWith("\n") ? text : `${text}\n`));
}

async function start(job, progress) {
  const base = new URL(`./python/${job.python}/`, self.location.href).href;
  const cdn = `${CDN}v${job.pyodide}/full/`;
  progress(`Starting Python ${job.python}. The first use downloads the interpreter (about 13 MB).`);
  const { loadPyodide } = await import(`${base}pyodide.mjs`);
  const pyodide = await loadPyodide({ indexURL: base, packageBaseUrl: cdn });
  if (pyodide.version !== job.pyodide) {
    throw new Error(`python/${job.python}/ holds Pyodide ${pyodide.version}, but ${job.pyodide} was expected`);
  }
  // Pyodide 0.27 ignores packageBaseUrl and looks for libraries beside the
  // interpreter. After start-up that is the only use it makes of indexURL.
  // Later releases record a cdnUrl, which is how the two are told apart.
  if (pyodide._api.config.cdnUrl === undefined) {
    pyodide._api.config.indexURL = cdn;
  }
  pyodide.setStdout({ write: collect, isatty: false });
  pyodide.setStderr({ write: collect, isatty: false });
  pyodide.FS.mkdirTree(ROOT);
  pyodide.FS.mkdirTree(SUPPORT);
  pyodide.FS.writeFile(`${SUPPORT}/_silver_wombat_figures.py`, FIGURE_BACKEND);
  pyodide.globals.set("_silver_wombat_send", () => sendOutput(true));
  pyodide.runPython(DRIVER, { filename: HERE });
  return pyodide;
}

// The Python side: finds what a program imports, runs it the way the
// `python` command would, and tidies up so the next run starts clean.
const DRIVER = `
def _silver_wombat():
    import ast, os, runpy, sys, time, traceback

    # A program that prints and then waits should show what it printed
    # before the wait, not after it.
    send = _silver_wombat_send
    wait = time.sleep

    def sleep(seconds):
        for stream in (sys.stdout, sys.stderr):
            try:
                stream.flush()
            except Exception:
                pass
        send()
        wait(seconds)

    time.sleep = sleep

    ROOT = "${ROOT}"
    HERE = "${HERE}"

    # Where matplotlib finds what show() does here (see FIGURE_BACKEND).
    sys.path.append("${SUPPORT}")
    os.environ.setdefault("MPLBACKEND", "module://_silver_wombat_figures")

    # Hand each line over as it is printed, so a terminal can show output
    # while the program runs, without claiming to be a terminal (which would
    # make some libraries print colour codes).
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(line_buffering=True)
        except Exception:
            pass

    def local_module(name, folders):
        parts = name.split(".")
        for folder in folders:
            base = os.path.join(folder, *parts)
            for candidate in (base + ".py", os.path.join(base, "__init__.py")):
                if os.path.isfile(candidate):
                    return candidate
        return None

    def imports(kind, value, cwd):
        """Top-level names of the libraries the program will import."""
        cwd = os.path.join(ROOT, cwd) if cwd else ROOT
        wanted, seen, queue = set(), set(), []
        folders = [cwd, ROOT]
        if kind == "file":
            queue.append(os.path.join(ROOT, value))
            folders.insert(0, os.path.dirname(queue[0]))
        elif kind == "module":
            found = local_module(value, folders)
            main = local_module(value + ".__main__", folders)
            if main or found:
                queue.append(main or found)
            else:
                wanted.add(value.split(".")[0])
                # A tool such as pytest goes on to import the project's files.
                for folder, subfolders, files in os.walk(cwd):
                    subfolders[:] = [d for d in subfolders if not d.startswith((".", "__"))]
                    queue.extend(os.path.join(folder, f) for f in files if f.endswith(".py"))
        sources = [value] if kind == "code" else []
        while queue or sources:
            if sources:
                source = sources.pop()
            else:
                path = queue.pop()
                if path in seen or len(seen) > 300:
                    continue
                seen.add(path)
                try:
                    with open(path, encoding="utf-8") as handle:
                        source = handle.read()
                except (OSError, UnicodeDecodeError):
                    continue
            try:
                tree = ast.parse(source)
            except (SyntaxError, ValueError):
                continue
            for node in ast.walk(tree):
                if isinstance(node, ast.Import):
                    names = [alias.name for alias in node.names]
                elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
                    names = [node.module]
                else:
                    continue
                for name in names:
                    found = local_module(name, folders)
                    if found:
                        queue.append(found)
                    elif not local_module(name.split(".")[0], folders):
                        wanted.add(name.split(".")[0])
        return sorted(wanted)

    def run(kind, value, args, cwd):
        cwd = os.path.join(ROOT, cwd) if cwd else ROOT
        os.makedirs(cwd, exist_ok=True)
        os.chdir(cwd)
        argv, search, before = sys.argv, list(sys.path), set(sys.modules)
        # A new reader, so input one program left unread is not given to the next.
        sys.stdin = open(0, encoding="utf-8", closefd=False)
        status = 0
        try:
            if kind == "file":
                path = os.path.join(ROOT, value)
                sys.path.insert(0, os.path.dirname(path))
                # A relative name, so argv[0] and error traces read as typed.
                runpy.run_path(os.path.relpath(path, cwd), run_name="__main__")
            elif kind == "module":
                sys.argv = [value, *args]
                sys.path.insert(0, cwd)
                runpy.run_module(value, run_name="__main__", alter_sys=True)
            elif kind == "code":
                sys.argv = ["-c", *args]
                sys.path.insert(0, cwd)
                exec(compile(value, "<string>", "exec"), {"__name__": "__main__"})
        except SystemExit as stop:
            if stop.code is None:
                status = 0
            elif isinstance(stop.code, int):
                status = stop.code
            else:
                print(stop.code, file=sys.stderr)
                status = 1
        except BaseException as error:
            trace = error.__traceback__
            while trace is not None and (
                trace.tb_frame.f_code.co_filename == HERE
                or os.path.basename(trace.tb_frame.f_code.co_filename) == "runpy.py"
                or trace.tb_frame.f_code.co_filename.startswith("<frozen runpy")
            ):
                trace = trace.tb_next
            traceback.print_exception(type(error), error, trace)
            status = 1
        finally:
            for stream in (sys.stdout, sys.stderr):
                try:
                    stream.flush()
                except Exception:
                    pass
            sys.argv = argv
            sys.path[:] = search
            # Forget the project's own modules, so an edited file is read again.
            for name in set(sys.modules) - before:
                origin = getattr(sys.modules.get(name), "__file__", None) or ""
                if origin.startswith(ROOT + "/"):
                    del sys.modules[name]
            os.chdir("/")
        return status

    return imports, run

_silver_wombat_imports, _silver_wombat_run = _silver_wombat()
del _silver_wombat
`;

function removeTree(FS, path) {
  for (const name of FS.readdir(path)) {
    if (name === "." || name === "..") continue;
    const child = `${path}/${name}`;
    if (FS.isDir(FS.lstat(child).mode)) {
      removeTree(FS, child);
      FS.rmdir(child);
    } else {
      FS.unlink(child);
    }
  }
}

function walk(FS, path, found) {
  for (const name of FS.readdir(path)) {
    if (name === "." || name === ".." || name === "__pycache__") continue;
    const child = `${path}/${name}`;
    const mode = FS.lstat(child).mode;
    if (FS.isDir(mode)) {
      walk(FS, child, found);
    } else if (FS.isFile(mode) && !name.endsWith(".pyc")) {
      found.push(child);
    }
  }
}

async function run(pyodide, job, progress) {
  const FS = pyodide.FS;
  removeTree(FS, ROOT);
  FS.mkdirTree(FIGURES);
  removeTree(FS, FIGURES);
  const given = new Map();
  for (const [path, text] of job.files) {
    const full = `${ROOT}/${path}`;
    FS.mkdirTree(full.slice(0, full.lastIndexOf("/")));
    FS.writeFile(full, text);
    given.set(path, text);
  }

  let downloadFailed = false;
  const libraries = {
    messageCallback: (message) => progress(message),
    errorCallback: (message) => {
      downloadFailed = true;
      say(message);
    },
  };
  let status = 0;
  try {
    if (job.install.length > 0) {
      progress(`Installing ${job.install.join(", ")}`);
      await pyodide.loadPackage("micropip", libraries);
      if (!("micropip" in pyodide.loadedPackages)) {
        throw new Error("the installer (micropip) could not be downloaded");
      }
      const micropip = pyodide.pyimport("micropip");
      try {
        // A Python list, not a JavaScript array seen from Python.
        const wanted = pyodide.toPy(job.install);
        try {
          await micropip.install(wanted);
        } finally {
          wanted.destroy();
        }
      } finally {
        micropip.destroy();
      }
    }
    if (job.entry.kind !== "nothing") {
      const findImports = pyodide.globals.get("_silver_wombat_imports");
      const wanted = findImports(job.entry.kind, job.entry.value, job.cwd);
      const names = wanted.toJs();
      wanted.destroy();
      findImports.destroy();
      if (names.length > 0) {
        await pyodide.loadPackagesFromImports(names.map((name) => `import ${name}`).join("\n"), libraries);
      }
    }
  } catch (error) {
    say(`Could not install: ${error.message ?? error}`);
    say("A library can be installed here if Pyodide has built it or it is pure Python. One that needs its own native code cannot run in a browser.");
    status = 1;
  }
  if (downloadFailed) {
    say("A library could not be downloaded. Libraries come from cdn.jsdelivr.net the first time they are used, so that needs a connection.");
  }

  if (status === 0 && job.entry.kind !== "nothing") {
    // Set afresh for every run, so input left unread by one program is not
    // handed to the next. With nothing piped in, input() reports end of file.
    let piped = job.stdin ?? null;
    pyodide.setStdin({
      stdin: () => {
        const text = piped;
        piped = null;
        return text;
      },
    });
    try {
      // Called from an async function so that programs using asyncio can
      // wait, in browsers that allow it.
      pyodide.globals.set("_silver_wombat_job", pyodide.toPy([job.entry.kind, job.entry.value, job.args, job.cwd]));
      status = await pyodide.runPythonAsync("_silver_wombat_run(*_silver_wombat_job)", { filename: HERE });
    } finally {
      pyodide.runPython("del _silver_wombat_job");
    }
  }

  const present = [];
  walk(FS, ROOT, present);
  const changed = [];
  const binary = [];
  const pictures = [];
  const seen = new Set();
  const decoder = new TextDecoder("utf-8", { fatal: true });
  for (const full of present) {
    const path = full.slice(ROOT.length + 1);
    seen.add(path);
    const bytes = FS.readFile(full);
    let text = null;
    if (bytes.length <= MAX_FILE_BYTES) {
      try {
        text = decoder.decode(bytes);
      } catch {
        text = null;
      }
    }
    const isNew = text === null || given.get(path) !== text;
    if (text === null) {
      binary.push(path);
    } else if (isNew) {
      changed.push([path, text]);
    }
    // A picture the program wrote (a plot, say) is handed over to be shown.
    const type = PICTURE_TYPES[path.slice(path.lastIndexOf(".") + 1).toLowerCase()];
    if (type && isNew && pictures.length < MAX_PICTURES && bytes.length <= MAX_PICTURE_BYTES) {
      pictures.push({ path, type, data: base64(bytes) });
    }
  }
  for (const path of given.keys()) {
    if (!seen.has(path)) changed.push([path, null]);
  }
  // Figures from plt.show().
  const figures = [];
  walk(FS, FIGURES, figures);
  for (const full of figures.sort((a, b) => a.length - b.length || a.localeCompare(b))) {
    const bytes = FS.readFile(full);
    if (pictures.length < MAX_PICTURES && bytes.length <= MAX_PICTURE_BYTES) {
      pictures.push({ path: full.slice(FIGURES.length + 1), type: "image/png", data: base64(bytes) });
    }
  }
  return { status, changed, binary, pictures };
}

// Pictures a program writes are shown under the terminal: up to this many
// from one run, each up to this size.
const PICTURE_TYPES = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp", svg: "image/svg+xml" };
const MAX_PICTURES = 6;
const MAX_PICTURE_BYTES = 4 * 1024 * 1024;

function base64(bytes) {
  let text = "";
  for (let at = 0; at < bytes.length; at += 0x8000) {
    text += String.fromCharCode.apply(null, bytes.subarray(at, at + 0x8000));
  }
  return btoa(text);
}

self.onmessage = async (event) => {
  const { id, job } = JSON.parse(event.data);
  const progress = (text) => self.postMessage(JSON.stringify({ id, progress: text }));
  chunks = [];
  outputBytes = 0;
  outputCut = false;
  unsent = "";
  allowance = BURST;
  refilled = Date.now();
  liveDecoder = new TextDecoder("utf-8");
  live = job.stream ? (text) => self.postMessage(JSON.stringify({ id, output: text })) : null;
  try {
    if (loadedPython !== null && loadedPython !== job.python) {
      throw new Error(`this worker holds Python ${loadedPython}; a new one is needed for ${job.python}`);
    }
    if (loading === null) {
      loading = start(job, progress);
      loadedPython = job.python;
    }
    let pyodide;
    try {
      pyodide = await loading;
    } catch (error) {
      loading = null;
      loadedPython = null;
      throw error;
    }
    const result = await run(pyodide, job, progress);
    const pieces = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.length, 0));
    let at = 0;
    for (const chunk of chunks) {
      pieces.set(chunk, at);
      at += chunk.length;
    }
    let output = new TextDecoder("utf-8").decode(pieces);
    if (outputCut) {
      const cut = `\n[output cut: ${outputBytes - MAX_OUTPUT_BYTES} or more bytes not shown]\n`;
      if (live !== null) unsent += cut;
      else output += cut;
    }
    sendOutput(true);
    self.postMessage(JSON.stringify({ id, outcome: { output, ...result } }));
  } catch (error) {
    sendOutput(true);
    self.postMessage(JSON.stringify({ id, error: String(error?.message ?? error) }));
  } finally {
    live = null;
  }
};
