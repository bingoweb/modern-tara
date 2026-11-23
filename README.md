# modern-tara

Kodda bir şeyi değiştirdiğimde, o satırların dilin ve çalışma ortamının en yeni kararlı sürümüne göre yazılıp yazılmadığını görmek istiyorum. "Bu kilit artık `System.Threading.Lock` olmalı", "bu `os.path.join` yerine `pathlib`", "bu döngü Rust'ta `let … else` ile yazılır" gibi şeyleri her seferinde aklımda tutmak yerine, bunları zaten bilen araçları çalıştıran tek bir komut yazdım.

modern-tara bir değişikliğe bakar, hangi dillere dokunduğunu ve projenin sürüm sabitlerini bulur, her dilin kendi modernleştirme kurallarını geçici bir kopyada çalıştırır ve yalnız değişen satırlardaki bulguları söyler. Taranan depoya hiçbir şey yazmaz.

## Neyi tarar

| Dil | Kullandığı araç | Okuduğu sürüm sabiti |
|---|---|---|
| C# / .NET | `dotnet format` (IDE ve CA kuralları) | `TargetFramework`, `LangVersion` |
| Rust | `cargo clippy` (pedantic ve modern deyim kuralları) | `edition`, `rust-version` |
| Python | `ruff` (UP, FURB, PTH, SIM) | `requires-python` |
| JavaScript / TypeScript | ESLint, eslint-plugin-unicorn, typescript-eslint, es-x | `engines.node` |
| PowerShell | PSScriptAnalyzer | betiği çalıştıran kabuk |
| CSS | stylelint, use-baseline, @eslint/css | `browserslist` |
| Go | `go fix` modernleştiricileri (cmd/fix, `go vet -vettool` ile) | go.mod `go` satırı |

Hiçbir kuralın yakalamadığı birkaç kalıp için ast-grep kuralları da var.

## Bulgu türleri

- **modern**: aynı işi, projenin şu anki sürümünde daha yeni ve daha kısa bir yapıyla yapmak mümkün.
- **needs-newer**: daha iyi yapı var ama sürüm sabitini yükseltmek gerekiyor; bulgu, hangi sabitin neye çıkacağını da söyler.
- **baseline**: kullanılan bir özellik, projenin hedeflediği tarayıcılarda ya da Node sürümünde henüz olmayabilir.

Her bulgunun bir de güven düzeyi var. Kural motorlarından gelenler kesin, sezgisel kalıplar düşük güvenli sayılır.

## Kullanım

```
modern-tara scan --repo . --range origin/main..HEAD
modern-tara scan --repo . --range origin/main..HEAD --gate
modern-tara scan --repo . --files src/app.py --all-lines
```

`--gate` yalnız kesin `modern` bulgu varsa 1 ile çıkar; `needs-newer` ve düşük güvenli bulgular uyarı olarak yazılır. Değişen bir dosya taranamazsa (o dilin aracı yoksa, atlandıysa ya da dosya okunamadıysa) `--gate` 2 ile çıkar; bunu bilerek kabul etmek için `--allow-skipped <dil,...|all>` verilir. `--range a...b`, `b` dalını `a` ile ortak atasından bu yana tarar. Ayrıntılar: [docs/USAGE.md](docs/USAGE.md).

Bazen eski biçim bilerek kalır: bir modül dizisi bitene kadar CommonJS, açılışı bekletmemesi gereken bir `.catch()` zinciri gibi. Bunları deponun kökündeki `modern-tara.toml` dosyasına kural, yol ve gerekçeyle yazıyorum (`# noqa`, `eslint-disable`, `#[allow]` gibi satır içi susturmalar bulguyu gizlemez); istenirse bir bitiş tarihi de veriliyor. Bu bulgular raporda `waived` olarak görünür ve kapıyı durdurmaz; süresi dolan istisnanın bulguları ise kendiliğinden yeniden sayılır.

```toml
[[waiver]]
rule = "unicorn/prefer-module"
paths = ["src/**/*.js"]
reason = "ES modül dizisi bitene kadar CommonJS"
expires = "2027-03-01"
```

## Kurulum

[Sürümler](https://github.com/bingoweb/modern-tara/releases) sayfasından işletim sistemine uygun zip'i indirip açın, sonra sabitlenmiş yardımcı araçları kurun:

```
scripts/install-tools.ps1   # Windows
scripts/install-tools.sh    # Linux
```

Araç sürümleri `tools.toml` içinde tek yerde durur.

## GitHub Actions

```yaml
- uses: bingoweb/modern-tara@v0.5.0
```

Eylem sürümü indirir, SHA256'sını doğrular, araçları kurar ve pull request ya da push aralığını `--gate` ile tarar; özeti iş akışı sayfasına yazar.

## Bilmeniz gerekenler

Rust ve C# taramaları projeyi derler. Bu, deponun `build.rs` dosyalarının, procedural makrolarının ve MSBuild hedeflerinin sizin kullanıcı yetkinizle çalışması demek; güvenmediğiniz bir depoda çalıştırmayın.

## Lisans

MIT
