"""Build the documentation with Python 3.11+ and no third-party packages."""

from html import escape
from pathlib import Path
import json
import re
import shutil

ROOT = Path(__file__).resolve().parent
OUT = ROOT / "_site"
SITE = "https://gitwatch.dpdns.org"
PAGES = json.loads((ROOT / "pages.json").read_text(encoding="utf-8"))
LABELS = {
    "en": {"skip": "Skip to content", "docs": "Documentation", "menu": "Contents", "onpage": "On this page", "next": "Next", "previous": "Previous", "edit": "Edit this page", "version": "Version 0.4", "notice": "A guide to the desktop app, file backups and synchronization.", "install": "Installation", "footer": "Keep a history of your work.", "group": ["Get started", "Task modes", "Reference"]},
    "zh-CN": {"skip": "跳到正文", "docs": "使用文档", "menu": "目录", "onpage": "本页内容", "next": "下一页", "previous": "上一页", "edit": "编辑此页", "version": "0.4 版本", "notice": "桌面应用、文件备份与同步使用指南。", "install": "安装说明", "footer": "为你的工作保留历史记录。", "group": ["开始使用", "任务模式", "参考"]},
}


def build():
    if OUT.exists():
        shutil.rmtree(OUT)
    OUT.mkdir()
    shutil.copytree(ROOT / "assets", OUT / "assets")
    for lang, labels in LABELS.items():
        (OUT / lang).mkdir()
        other = "zh-CN" if lang == "en" else "en"
        for i, page in enumerate(PAGES):
            slug = page["slug"]
            body = (ROOT / "content" / lang / f"{slug}.html").read_text(encoding="utf-8")
            headings = re.findall(r'<h2 id="([^"]+)">(.*?)</h2>', body)
            toc = "".join(f'<a href="#{anchor}">{title}</a>' for anchor, title in headings)
            nav = ""
            for group in range(3):
                nav += f'<p class="nav-group">{labels["group"][group]}</p>'
                for entry in PAGES:
                    if entry["group"] == group:
                        current = ' aria-current="page"' if entry["slug"] == slug else ""
                        label = ("Overview" if lang == "en" else "概览") if entry["slug"] == "index" else entry[lang]["title"]
                        nav += f'<a href="{entry["slug"]}.html"{current}>{label}</a>'
            pager = ""
            for j, direction in [(i - 1, "previous"), (i + 1, "next")]:
                if 0 <= j < len(PAGES):
                    entry = PAGES[j]
                    pager += f'<a class="{direction}" href="{entry["slug"]}.html"><small>{labels[direction]}</small>{entry[lang]["title"]} <span aria-hidden="true">{"→" if direction == "next" else "←"}</span></a>'
            title = escape(page[lang]["title"])
            description = escape(page[lang]["description"], quote=True)
            html = f'''<!doctype html>
<html lang="{lang}">
<head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · gitwatch</title><meta name="description" content="{description}">
<meta name="theme-color" content="#163f3b">
<link rel="canonical" href="{SITE}/{lang}/{slug}.html">
<link rel="alternate" hreflang="en" href="{SITE}/en/{slug}.html">
<link rel="alternate" hreflang="zh-CN" href="{SITE}/zh-CN/{slug}.html">
<link rel="icon" href="../assets/favicon.svg" type="image/svg+xml">
<link rel="stylesheet" href="../assets/style.css"><script src="../assets/site.js" defer></script>
</head>
<body>
<a class="skip" href="#main">{labels['skip']}</a>
<header class="topbar"><div class="topbar-inner">
<a class="brand" href="index.html"><img src="../assets/favicon.svg" width="28" height="28" alt="">gitwatch <span>{labels['docs']}</span></a>
<nav aria-label="{'Site' if lang == 'en' else '站点'}"><a class="language" href="../{other}/{slug}.html" lang="{other}" hreflang="{other}">{'简体中文' if other == 'zh-CN' else 'English'}</a><a href="https://github.com/0x676e67/gitwatch">GitHub <span aria-hidden="true">↗</span></a></nav>
</div></header>
<div class="layout">
<aside class="sidebar"><details class="mobile-nav" open><summary>{labels['menu']}</summary><nav aria-label="{labels['menu']}">{nav}</nav></details><div class="sidebar-note"><span class="dot"></span>{labels['version']}<br><code>v0.4.0</code></div></aside>
<main id="main" tabindex="-1">
<div class="version-note"><strong>{labels['version']}</strong><span>{labels['notice']} <a href="install.html">{labels['install']} →</a></span></div>
<p class="eyebrow">{labels['group'][page['group']]}</p><h1>{title}</h1><p class="lead">{description}</p>
<article>{body}</article>
<nav class="pager" aria-label="{'Pages' if lang == 'en' else '页面'}">{pager}</nav>
<footer><span>{labels['footer']}</span><a href="https://github.com/0x676e67/gitwatch/blob/gh-pages/content/{lang}/{slug}.html">{labels['edit']} ↗</a></footer>
</main>
<aside class="toc"><p>{labels['onpage']}</p><nav aria-label="{labels['onpage']}">{toc}</nav></aside>
</div>
</body></html>'''
            (OUT / lang / f"{slug}.html").write_text(html, encoding="utf-8")
    (OUT / "index.html").write_text('''<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta http-equiv="refresh" content="0;url=en/index.html"><title>gitwatch documentation</title></head><body><h1>gitwatch</h1><p><a href="en/index.html">English documentation</a> · <a href="zh-CN/index.html" lang="zh-CN">简体中文文档</a></p></body></html>''', encoding="utf-8")
    (OUT / "404.html").write_text('''<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Page not found · gitwatch</title></head><body><h1>Page not found / 页面不存在</h1><p><a href="/en/index.html">English documentation</a> · <a href="/zh-CN/index.html" lang="zh-CN">简体中文文档</a></p></body></html>''', encoding="utf-8")
    for lang, other in [("en", "zh-CN"), ("zh-CN", "en")]:
        title = "The terminal UI has been removed" if lang == "en" else "终端界面已移除"
        (OUT / lang / "tui.html").write_text(f'''<!doctype html><html lang="{lang}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta http-equiv="refresh" content="0;url=desktop.html"><link rel="canonical" href="{SITE}/{lang}/desktop.html"><title>{title} · gitwatch</title></head><body><h1>{title}</h1><p><a href="desktop.html">{'Open the desktop guide' if lang == 'en' else '查看桌面指南'}</a> · <a href="../{other}/desktop.html">{'简体中文' if other == 'zh-CN' else 'English'}</a></p></body></html>''', encoding="utf-8")
    (OUT / ".nojekyll").touch()
    print(f"Built {len(PAGES) * len(LABELS)} documentation pages in {OUT}")


if __name__ == "__main__":
    build()
