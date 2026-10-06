# chkdsk

Painel de saúde de discos para Windows, escrito em Rust, com interface de terminal em português brasileiro (pt_BR). Consulta telemetria SMART/Health, apresenta uma nota heurística conservadora e mantém histórico local para tendências de desgaste. O aplicativo não depende de serviços externos durante a execução.

> Este projeto não é o `chkdsk` do Windows. Não verifica nem repara sistemas de arquivos, não agenda reparos e não substitui `chkdsk.exe`; também não executa varredura de superfície. Para evitar conflito com a ferramenta do sistema, inicie o executável deste projeto pelo caminho completo.

## Executar

Requer Windows 10/11 e um terminal compatível com TUI. Releases incluem executáveis para Windows x64, ARM64 e x86. O runtime não requer Rust, mise ou acesso à rede. A interface usa a paleta Moon do [TokyoNight.nvim v4.14.1](https://github.com/folke/tokyonight.nvim/releases/tag/v4.14.1), fixada no commit `545d72c`: composição compacta, plana e densa, com linhas informativas horizontais listradas, abas de disco horizontais não colapsáveis com rolagem quando excedem o espaço, e botões semânticos preenchidos por cor, sem colchetes.

## Pontuação e limites

- A nota de 0 a 100 é determinística, mas não representa probabilidade de falha nem certificação do disco. Com evidência utilizável, notas acima de 50 são classificadas como saudáveis e notas até 50 como atenção; sem evidência utilizável, o estado é **SEM DADOS**.
- Para ATA/SATA, o aplicativo consulta SMART READ DATA e tenta ler SMART READ THRESHOLDS; usa pass-through ATA nativo, com fallback para o IOCTL SMART legado quando necessário. Os atributos lidos são retidos com valores normalizados atuais/piores e valores brutos; quando há limiares declarados válidos, compara-os com o valor normalizado atual. Atingir qualquer limiar limita a nota a 20, mas só indica falha se a flag pré-falha (bit 0) estiver marcada ou se o estado global SMART reportar falha; ficar até 10 pontos normalizados acima do limiar limita a nota a 70, sem marcar falha. O estado SMART global de “sem falha” é evidência grosseira, de baixa confiança, não uma certificação de saúde. Não há atualmente perfil de interpretação bruta nem deduções específicas para HDD: dados brutos de IDs de setores, temperatura, horas ou outros atributos não são interpretados; as regras de limiares normalizados acima continuam válidas.
- O desgaste usado nativo do NVMe, e o atributo ATA SMART 202 somente nos perfis exatos suportados de modelos Crucial MX300, MX500 e BX500, impõe à nota um teto de `100 − desgaste usado`. O perfil ATA interpreta o raw completo de 48 bits como percentual usado e limita valores acima de 100 a 100%. Assim, 100% de desgaste pode resultar em nota zero e aviso sem, por si só, classificar o disco como falho; nota baixa ou contador histórico isolado também não significa falha física. Não há mapeamento linear verificado para o atributo Samsung 177, nem inferência de vida útil a partir de outros IDs brutos.
- O resultado da avaliação também inclui motivos de aviso estruturados. No NVMe, `media_errors` acima de zero (erros históricos de mídia/integridade de dados) limita a nota a 70 e aplica dedução determinística de 5 pontos por ordem de grandeza decimal, limitada a 30 pontos; o bit 5, de região de memória persistente (PMR), gera aviso e limita a nota a 70. Esses avisos, isoladamente, não classificam o disco como falho.
- A temperatura e as horas de funcionamento são exibidas somente quando fornecidas por campos de telemetria decodificados; atualmente são populadas pelo log de saúde NVMe, não deduzidas de atributos ATA brutos genéricos.
- A ETA de desgaste é uma extrapolação indicativa até 100% de desgaste usado, baseada no segmento atual de histórico desde a leitura mais antiga após o último reset/regressão do contador até a mais recente. Exige aumento positivo observado ao longo do tempo; leituras iguais, regressivas ou dados insuficientes não estabelecem tendência. HDDs não recebem previsão. A ETA não prevê falhas nem garante vida útil.
- O histórico fica em `%APPDATA%\chkdsk\historico.json` (ou na pasta temporária do usuário quando `%APPDATA%` não está definido). Mantém até 35 dias por disco: amostras por hora nos sete dias mais recentes e por dia antes disso. Uma regressão do contador de desgaste descarta o histórico anterior daquele disco para iniciar uma nova linha de base.
- Na inicialização normal, a partir da versão 0.0.2, o aplicativo solicita elevação pelo UAC nativo do Windows e se reinicia com privilégios de administrador após a confirmação. `chkdsk.exe --version` apenas imprime a versão e não solicita elevação. Windows, driver, controlador ou adaptador USB ainda podem bloquear consultas SMART; elevar privilégios não torna disponível uma telemetria que o dispositivo ou controlador não fornece.

## Controles

- `←` / `→` ou `Tab` / `Shift+Tab`: alternar entre discos.
- `↑` / `↓` ou `j` / `k`: rolar o conteúdo do painel.
- Clique em uma aba para selecionar o disco; a roda do mouse rola o painel ou, sobre as abas, alterna discos. Os botões auxiliares aparecem quando há espaço vertical suficiente.
- Clique no botão de saída para encerrar; em larguras estreitas, ele aparece como `×`.
- `q` ou `Esc`: sair.
- Abaixo de 17 linhas de altura, o rodapé de botões fica oculto; use a roda do mouse no painel/abas ou o teclado. Abaixo de 11 linhas, cabeçalho e abas usam uma linha compacta.

## Desenvolvimento

O ambiente de desenvolvimento usa [mise](https://mise.jdx.dev/) para provisionar Rust estável, `rustfmt`, Clippy, Python 3.12, `prek` e uma distribuição LLVM portátil fixada no projeto. LLVM não é necessário para executar o aplicativo. A compilação Windows requer os componentes do Visual Studio Build Tools (toolset MSVC e Windows SDK) para x64, x86 e ARM64; mise instala os componentes Rust, mas não substitui o linker/SDK MSVC.

Na raiz do repositório, em PowerShell:

```powershell
mise install
mise exec -- cargo run
```

Validação e compilação:

```powershell
mise run fmt
mise run clippy
mise run tests
mise run compile
mise run validation
mise run setup-hook
mise run release
```

`validation` confere formatação, regras de release, linkers MSVC, Clippy, testes x64 e compilação nas três arquiteturas. `setup-hook` instala o hook local do `prek`, que verifica `rustfmt`. `release` gera builds otimizados; também existem `release-x64`, `release-arm64` e `release-x86`.

## Releases

Tags estáveis seguem exatamente `v{versão}` e não aceitam versão SemVer pré-lançamento. Para a versão 0.0.2, a tag estável é `v0.0.2` e builds de desenvolvimento usam `v0.0.2-dev-{sha7}` (os sete primeiros caracteres hexadecimais do commit, em minúsculas). Os executáveis têm estes nomes; o sufixo de desenvolvimento aparece somente nesses builds:

```text
chkdsk-0.0.2[-dev-{sha7}]-windows-x64.exe
chkdsk-0.0.2[-dev-{sha7}]-windows-arm64.exe
chkdsk-0.0.2[-dev-{sha7}]-windows-x86.exe
```

`mise run release-check` valida localmente regras SemVer, nomes dos artefatos e mapeamentos de arquitetura sem publicar.
