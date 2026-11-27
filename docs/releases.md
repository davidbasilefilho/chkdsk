# Distribuição do disksonar

Cada release no formato atual contém três executáveis nativos distintos e `SHA256SUMS.txt`. Os nomes seguem `disksonar-{versão}[-dev-{sha7}]-windows-{perfil}.exe` com os perfis `arm64`, `x64` e `x64-baseline`.

| Perfil | Alvo |
| --- | --- |
| `arm64` | AArch64 MSVC genérico com NEON |
| `x64` | x86-64-v3 com AVX/AVX2 e suporte do sistema operacional ao estado AVX |
| `x64-baseline` | x86-64 genérico com SSE2, sem exigência de AVX |

C/C++ usa o MSVC para a arquitetura correspondente. Em x64, a compilação usa `/arch:AVX2` ou `/arch:SSE2`, conforme o perfil. [Geração de código Rust](https://doc.rust-lang.org/rustc/codegen-options/index.html#target-cpu) · [Arquiteturas MSVC](https://learn.microsoft.com/en-us/cpp/build/reference/arch-x64)

## Instalar com mise

Use um checkout confiável de [disksonar em main](https://github.com/davidbasilefilho/disksonar/tree/main), com mise, Bun e GitHub CLI autenticado para ler Actions e Contents. Selecione o canal e o perfil:

```powershell
mise run channel-install dev x64-baseline
mise run channel-install pr arm64
mise run channel-install latest x64
```

Para um PR específico, troque `pr` por `pr-N`, com o número do PR no lugar de `N`. O instalador verifica os três arquivos e o manifesto, instala o perfil solicitado e imprime um comando `mise x` com o caminho local da versão. Use a linha anterior se precisar voltar a uma versão já instalada. [Versões locais do mise](https://mise.jdx.dev/dev-tools/versions.html)

| Canal | Origem |
| --- | --- |
| `dev` | Pré-release do HEAD atual da branch `dev`, vinculada à validação desse push |
| `pr-N` | Pré-release do HEAD atual de um PR aberto e fora de rascunho, no próprio repositório e com base `main` ou `dev` |
| `pr` | O HEAD publicado elegível com a data de commit mais recente. Em empate, o maior número de PR vence |
| `latest` | Release estável aprovada no ponteiro `channels/latest.json` de `main` |

O instalador reconfirma a seleção após o download. A ordem da página de Releases e a hora de conclusão da CI não determinam nenhum canal. Releases históricas anteriores ao formato atual continuam disponíveis na [página de Releases](https://github.com/davidbasilefilho/disksonar/releases).

## Publicar

O workflow `validation` compila os três perfis e os executa em runners Windows x64 e ARM64 com fixtures. O ZIP da CI passa o mesmo build ao workflow confiável `preview release` de `main`. O publicador valida origem, HEAD, jobs, SHA e manifesto. Ele não executa fonte ou binários do PR. Pré-releases válidas recebem tags imutáveis `v{versão}-dev-{sha40}` ou `v{versão}-pr-{número}-{sha40}`. Forks e PRs fechados ou em rascunho não são elegíveis.

O publicador também aceita em `main` o ID e a tentativa de uma validação concluída para recuperar uma publicação pendente com o mesmo pacote. Para reverter `dev`, reverta os commits necessários na branch e valide o novo HEAD. [Gatilho workflow_run](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#workflow_run)

A publicação estável é manual e depende de aprovação explícita. Proteja `main` e as tags estáveis contra reescrita e configure o environment `stable` com aprovação e restrição a `main`. Revise a tag SemVer e o SHA de `main` no ponteiro `channels/latest.json`. Depois, dispare `stable release` em `main` com a tag e o SHA aprovados. O rollback do canal estável é feito por meio do ponteiro para uma release já publicada e elegível.
