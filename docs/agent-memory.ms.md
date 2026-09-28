# Pengindeksan Memori Merentas Ejen

[English](agent-memory.md) · [中文](agent-memory.zh.md) · [繁體中文](agent-memory.zh-TW.md) · [日本語](agent-memory.ja.md) · [한국어](agent-memory.ko.md) · [Русский](agent-memory.ru.md) · [Français](agent-memory.fr.md) · [Deutsch](agent-memory.de.md) · [Português](agent-memory.pt.md) · [Español](agent-memory.es.md) · [العربية](agent-memory.ar.md) · [Italiano](agent-memory.it.md) · [Ελληνικά](agent-memory.el.md) · [ไทย](agent-memory.th.md) · **Bahasa Melayu**

## Penggunaan

```bash
# Hanya memori terpilih dan ringkasan.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# Sertakan secara eksplisit sejarah sembang/sesi mentah yang sepadan.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# Cari hanya memori yang dikaitkan dengan projek semasa.
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

Pemasang menjalankan borang terpilih secara lalai. Gunakan `--skip-agent-memory` untuk melumpuhkannya atau `--include-raw-memory` untuk memilih masuk sejarah mentah.

## Sumber

Penemuan dikehadkan kepada projek kanonik yang dipilih:

| Ejen | Sumber terpilih lalai | Pilihan masuk mentah |
|---|---|---|
| Trae | profil pengguna, memori projek sepadan, ringkasan topik, ringkasan sesi | tiada apa melebihi ringkasan tersebut |
| Codex | `MEMORY.md`, `memory_summary.md`, ringkasan pelancaran sepadan | JSONL pelancaran mentah dipautkan dan `raw_memories.md` |
| Zcode | punca memori dipetakan kepada projek oleh metadata masa jalan Zcode | fail pelancaran/ejen sepadan |
| Gemini CLI | tiada | sejarah projek dipetakan dan direktori sembang |
| Claude Code | direktori memori projek sepadan | fail sesi yang mengenal pasti projek |

Direktori sesi Argos/SRE dikecualikan dengan sengaja kerana ia memerlukan aliran kerja diagnostik Argos dan bukannya pengindeksan fail pukal. AWI tidak pernah mengimbas keseluruhan direktori rumah.

## Metadata dan Kedudukan

Setiap hasil memori termasuk:

- Ejen sumber;
- lapisan: `user_profile`, `project_summary`, `topic_summary`, `session_summary`, `memory_note` atau `raw_history`;
- `name` dan `description` YAML frontmatter pilihan;
- punca ruang kerja kanonik dan kunci projek pembekal apabila tersedia;
- ID sesi apabila boleh diterbitkan;
- masa pemerhatian dan bendera sejarah mentah.

Apabila `context_path` atau penapis punca projek dibekalkan, memori daripada projek lain ditolak. Memori projek tepat disusun mendahului memori global. Padanan nama fail tepat dan `name` frontmatter menerima rangsangan metadata paling kuat; pertindihan penerangan memberikan rangsangan yang lebih kecil. Pertanyaan berbentuk pengecam menurunkan ringkasan luas `MEMORY.md`/projek melainkan ringkasan itu sendiri sepadan tepat dengan entiti. Susunan lapisan biasa kekal tidak berubah untuk pertanyaan luas, ringkasan terpilih disusun mendahului sejarah mentah, dan kebaruan hanyalah pemecah seri kecil. Salinan dengan kandungan serupa daripada Ejen berbeza dilipat selepas susunan. Capaian memori diaktifkan dengan `--kind agent_memory`. Dokumen memori menggunakan indeks Tantivy khusus supaya perbendaharaan katanya tidak boleh mengubah statistik IDF atau susunan kod/data biasa.

## Penghuraian dan Keselamatan

Memori Markdown diindeks sebagai teks terhad. Memori JSON/JSONL dinormalkan kepada medan berkaitan manusia seperti niat, tindakan, hasil, fakta dipelajari, peranan, mesej dan kandungan; ID pengangkutan dan metadata ringkasan dalaman diketepikan daripada dokumen carian. Memori JSONL tidak dihantar ke pemprofilan DuckDB.

Pemeriksaan saiz biasa, UTF-8, abaikan dan nama fail sensitif masih terpakai. Kelayakan atau kunci persendirian yang sangat dipercayai menyebabkan keseluruhan fail memori menjadi metadata sahaja. Sejarah mentah dilumpuhkan melainkan diminta secara eksplisit, dan fail mentah besar kekal metadata sahaja di bawah had kandungan yang dikonfigurasikan.

`awi memory` mengekalkan punca projek kanonik dan dasar sejarah mentah. Setiap kitaran pengeluar menemui semula sumber Ejen projek itu sebelum menerbitkan, jadi direktori projek pembekal baharu muncul tanpa memasang semula AWI. Punca yang didaftarkan sebelum ini juga diselaraskan apabila ia hilang, mengelakkan memori yang dipadamkan daripada kekal boleh dicari. JSONL ringkasan yang dilampirkan disegarkan pada penyelarasan berkala seterusnya; fail mentah berat lampiran tidak mencetuskan pembinaan semula petikan setiap penulisan.

`awi status --json` mendedahkan objek `memory` dengan kiraan projek/sumber berdaftar, fail aktif, generasi memori terbaharu dan susulan generasi relatif, `stale_files`/`missing_files` sistem fail, susulan sumber maksimum dan usia sumber tertua. Kiraan fail lapuk dan hilang sistem fail adalah isyarat kesegaran yang berwibawa; susulan generasi rendah sahaja tidak membuktikan memori luaran adalah semasa.

## Pengesahan Pembangunan

Larian Bona terpencil menemui 35 punca berkaitan projek dan mengindeks 489 fail terpilih daripada Codex, Trae dan Zcode dengan sifar kegagalan pengekstrakan; sembang mentah Gemini kekal dikecualikan. Set capaian kod/data 16 kes sedia ada kekal stabil bit-untuk-bit pada Recall@10 `1.0`, MRR `0.6006`, nDCG@10 `0.6950` dan kadar lapuk `0`. P95 binaan keluaran ialah `24.06 ms`. Set ini masih `candidate_pending_dual_review`, bukan pintu keluaran beku. Jangan laporkan ketepatan/panggilan semula rasmi, Inspect@K atau MRR sehingga dua penilai berbeza melabelkan set pertanyaan secara bebas dan mengadili pertikaian.
