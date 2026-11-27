# disksonar

O disksonar mostra a saúde dos discos no Windows. Ele lê os dados SMART e os contadores de armazenamento que o dispositivo disponibiliza, mostra a origem das medições e guarda histórico local de desgaste.

A janela funciona em português do Brasil ou inglês, conforme os idiomas de exibição do Windows. O aplicativo funciona offline.

## Como instalo?

A estável atual é a `v0.0.1`. Para instalar, siga o [guia de download direto e verificação SHA-256](docs/releases.md#instalar-a-estável-v001).

Os builds atuais de `dev` e de PRs oferecem estes perfis na [página de Releases](https://github.com/davidbasilefilho/disksonar/releases).

| Perfil | Sufixo do executável | Quando usar |
| --- | --- | --- |
| ARM64 | `-windows-arm64.exe` | Windows em ARM64 |
| x64 | `-windows-x64.exe` | Processador x64 com suporte a x86-64-v3 e AVX2 |
| x64-baseline | `-windows-x64-baseline.exe` | Processador x64 antigo ou quando houver dúvida sobre suporte a AVX2 |

O aplicativo requer Windows 10 ou 11 e GPU compatível com DirectX 12 ou Vulkan. Os executáveis são independentes de Rust, Bun e mise. Para testar o próximo build da branch `dev`, veja a [instalação por canal com mise](docs/releases.md#instalar-com-mise).

## O que posso ver?

- **Estado do disco.** O painel reúne o diagnóstico, desgaste, temperatura, horas de funcionamento e última leitura. Em **Informações avançadas** ficam as fontes e os atributos ATA brutos.
- **Tendência de desgaste.** O histórico só produz uma projeção quando há leituras suficientes e uma taxa plausível. A projeção descreve o consumo de resistência nominal.
- **Aparência.** Tema e animações seguem o Windows por padrão. Você pode escolher ambas as preferências em **Configurações**.
- **Diagnóstico somente leitura.** O disksonar consulta os discos. Para investigar ou reparar um sistema de arquivos, use o [CHKDSK do Windows](https://learn.microsoft.com/windows-server/administration/windows-commands/chkdsk).

Veja [como interpretar as leituras e usar os controles](docs/diagnostico.md).

## Desenvolvimento

Em um checkout no Windows, instale [mise](https://mise.jdx.dev/) e os componentes MSVC do Visual Studio Build Tools. Na raiz do repositório, execute:

```powershell
mise install
mise exec -- cargo run
mise run validation
```

O [guia de desenvolvimento](docs/desenvolvimento.md) cobre hooks, testes e capturas com fixtures. O [guia de releases](docs/releases.md) cobre os perfis, canais e a promoção estável.
