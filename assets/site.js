"use strict";

const chinese = document.documentElement.lang === "zh-CN";
const menu = document.querySelector(".mobile-nav");
const narrow = window.matchMedia("(max-width: 1000px)");
function updateMenu() { if (menu) menu.open = !narrow.matches; }
updateMenu();
narrow.addEventListener("change", updateMenu);

const status = document.createElement("div");
status.className = "copy-status";
status.setAttribute("role", "status");
document.body.append(status);
let clearStatus;
for (const code of document.querySelectorAll("pre > code")) {
  const button = document.createElement("button");
  button.className = "copy";
  button.type = "button";
  button.textContent = chinese ? "复制" : "Copy";
  button.setAttribute("aria-label", chinese ? "复制命令" : "Copy command");
  button.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(code.textContent);
      status.textContent = chinese ? "已复制" : "Copied to clipboard";
    } catch {
      status.textContent = chinese ? "请选中命令后复制。" : "Select the command and copy it manually.";
    }
    window.clearTimeout(clearStatus);
    clearStatus = window.setTimeout(() => { status.textContent = ""; }, 3000);
  });
  code.parentElement.append(button);
}
