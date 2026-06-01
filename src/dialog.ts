const overlay = () => document.getElementById("app-dialog-overlay")!;
const msgEl = () => document.getElementById("app-dialog-message")!;
const inputEl = () => document.getElementById("app-dialog-input")! as HTMLInputElement;
const okBtn = () => document.getElementById("app-dialog-ok")!;
const cancelBtn = () => document.getElementById("app-dialog-cancel")!;

function show(message: string, inputMode: boolean, defaultValue: string): Promise<string | null> {
  return new Promise((resolve) => {
    const o = overlay();
    const inp = inputEl();
    msgEl().textContent = message;

    if (inputMode) {
      inp.classList.remove("hidden");
      inp.value = defaultValue;
    } else {
      inp.classList.add("hidden");
    }

    o.classList.remove("hidden");
    if (inputMode) {
      inp.focus();
      inp.select();
    } else {
      okBtn().focus();
    }

    let done = false;
    const finish = (val: string | null) => {
      if (done) return;
      done = true;
      o.classList.add("hidden");
      cleanup();
      resolve(val);
    };

    const onOk = () => finish(inputMode ? inp.value : "");
    const onCancel = () => finish(null);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Enter") { e.preventDefault(); onOk(); }
      else if (e.key === "Escape") { e.preventDefault(); onCancel(); }
    };
    const onOverlayClick = (e: MouseEvent) => {
      if (e.target === o) onCancel();
    };

    okBtn().addEventListener("click", onOk);
    cancelBtn().addEventListener("click", onCancel);
    o.addEventListener("keydown", onKey);
    o.addEventListener("mousedown", onOverlayClick);

    function cleanup() {
      okBtn().removeEventListener("click", onOk);
      cancelBtn().removeEventListener("click", onCancel);
      o.removeEventListener("keydown", onKey);
      o.removeEventListener("mousedown", onOverlayClick);
    }
  });
}

export function appPrompt(message: string, defaultValue = ""): Promise<string | null> {
  return show(message, true, defaultValue);
}

export function appConfirm(message: string): Promise<boolean> {
  return show(message, false, "").then((v) => v !== null);
}
