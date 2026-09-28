# Pengindeksan Pengetahuan Ejen

[English](agent-knowledge.md) · [中文](agent-knowledge.zh.md) · [繁體中文](agent-knowledge.zh-TW.md) · [日本語](agent-knowledge.ja.md) · [한국어](agent-knowledge.ko.md) · [Русский](agent-knowledge.ru.md) · [Français](agent-knowledge.fr.md) · [Deutsch](agent-knowledge.de.md) · [Português](agent-knowledge.pt.md) · [Español](agent-knowledge.es.md) · [العربية](agent-knowledge.ar.md) · [Italiano](agent-knowledge.it.md) · [Ελληνικά](agent-knowledge.el.md) · [ไทย](agent-knowledge.th.md) · **Bahasa Melayu**

Sejarah projek merentas Ejen dikendalikan secara berasingan sebagai `agent_memory`; lihat [Pengindeksan Memori Merentas Ejen](agent-memory.md).

## Jenis Dokumen

| Fail | Jenis AWI | Metadata berstruktur |
|---|---|---|
| `AGENTS.md` | `agent_instructions` | direktori skop, kedalaman keutamaan, tajuk, rujukan tempatan |
| `SKILL.md` | `agent_skill` | `name` dan `description` YAML, tajuk, rujukan tempatan |

Teks Markdown terhad penuh kekal boleh dicari. `workspace_inspect` mengembalikan metadata berstruktur bersama dengan metadata fail biasa dan petikan kandungan.

## Penemuan

Penyelarasan ruang kerja biasa mengindeks fail `AGENTS.md` dan `SKILL.md` di bawah ruang kerja itu, tertakluk kepada `.gitignore`, `.awiignore` dan pengecualian lalai AWI.

Pemasang larian pertama turut menemui:

- fail `AGENTS.md` dalam direktori moyang di atas ruang kerja yang dipilih;
- manifes `SKILL.md` di bawah direktori projek dan pengguna yang diketahui untuk klien Ejen yang disokong.

Setiap dokumen luaran didaftarkan sebagai punca fail tunggal. AWI tidak mengindeks keseluruhan direktori rumah atau kandungan penuh pakej Skill global. `--skip-agent-knowledge` melumpuhkan penemuan tambahan ini.

Pautan Markdown tempatan daripada dokumen Ejen direkodkan hanya apabila sasaran mereka wujud dan kekal di dalam punca dokumen. Fail yang dirujuk boleh dicari apabila ia sudah diliputi oleh punca ruang kerja; jika tidak, laluan rujukan dikembalikan sebagai metadata untuk pemeriksaan eksplisit dengan alat fail lain.

## Skop dan Kedudukan

Hantar `context_path` kepada `workspace_search` apabila menyelesaikan arahan repositori:

```bash
awi search "build and test rules" \
  --kind agent_instructions \
  --context-path /workspace/service/src/main.rs \
  --json
```

AWI mengecualikan fail `AGENTS.md` yang skopnya bukan moyang laluan konteks. Fail yang terpakai disusun mengikut kedalaman skop supaya arahan terdekat didahulukan. Selepas susunan ini, dokumen Ejen dengan kandungan yang sama dilipat, yang menyekat pendua Skill dan worktree yang disalin tanpa kehilangan arahan terdekat yang terpakai.

Gunakan `--kind agent_skill` untuk capaian berfokus Skill. Dokumen Ejen dikecualikan daripada carian kod/data biasa; lorong Ejen khusus diaktifkan oleh penapis jenis Ejen atau `context_path`.

## Keselamatan

Dokumen Ejen kekal tertakluk kepada pemeriksaan saiz biasa, UTF-8, pautan simbolik dan nama fail sensitif. Corak kunci persendirian dan token pembekal yang sangat dipercayai dalam kandungannya menyebabkan hasil metadata sahaja dengan `metadata_only_sensitive_content`; kandungan, pratonton dan metadata Ejen yang dihuraikan tidak disimpan.

Frontmatter Skill yang cacat direkodkan sebagai kegagalan pengekstrakan, manakala kandungan Markdown baharu masih menggantikan sebarang teks terindeks yang lebih lama. Ini mengelakkan penyediaan kandungan lapuk apabila hanya penghuraian metadata berstruktur gagal.

## Pengesahan Pembangunan

Pemasangan terpencil pada persekitaran Bona semasa menemui 136 dokumen Ejen kanonik dengan sifar kegagalan pengekstrakan. Pertanyaan asap yang disasarkan memilih `wukong-dag-failure-debugger` dahulu untuk pertanyaan kegagalan Wukong DAG/LogID dan `AGENTS.md` Bona untuk konteks sumber AWI.

Pada set capaian pembangunan 16 kes sedia ada, menambah dokumen Ejen ini mengekalkan Recall@10 pada `1.0` dan kadar hasil lapuk pada `0`; MRR berubah daripada `0.5975` kepada `0.6027`, nDCG@10 daripada `0.6922` kepada `0.6969`, dan P95 binaan keluaran daripada `8.07 ms` kepada `25.88 ms`. Latensi kekal di bawah sasaran carian hibrid `150 ms`. Set pembangunan ini masih menunggu semakan berganda dan bukan penanda aras keluaran beku.
