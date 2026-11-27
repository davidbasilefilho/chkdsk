# Desenvolver o disksonar

O `mise.toml` define as tarefas e provisiona Rust, Bun, hk e a distribuição LLVM usada no build. O Visual Studio Build Tools fornece o SDK e o linker MSVC para x64 e ARM64. O runtime da GUI não requer essas ferramentas.

```powershell
mise install
mise run validation
mise run setup-hook
```

A validação executa formatação, verificações de empacotamento, checagem dos linkers, Clippy, testes em Windows x64 e compilação nas duas arquiteturas. O hook hk executa `mise run fmt` e `mise run clippy`. Se um checkout antigo ainda usa o hook do prek, execute `prek uninstall` antes de instalar o hk.

Rune é fixado pelo `rev` de `Cargo.toml`. No Linux, instale `libxkbcommon-dev` e `libwayland-dev` para compilar. Para capturas sem GPU física, use Mesa Vulkan com `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.json`. A coleta nativa é exclusiva do Windows.

## Capturas com fixtures

```powershell
mise exec -- cargo run --features scenes -- --scene settings --snap disksonar-configuracoes.png --size 440x640 --theme dark
```

Além de `settings`, as cenas incluem `saudavel`, `atencao`, `indisponivel`, `carregando`, `falha`, `vazio`, `procurando`, `descoberta` e `macrovip`. As cenas não iniciam a coleta de discos e não pedem elevação.
