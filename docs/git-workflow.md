# Git 工作流（GitHub Flow）

本项目采用 [GitHub Flow](https://docs.github.com/en/get-started/using-github/github-flow)：

```
main ─────●──────●──────────●──────●──────▶
           \      \          ↑
            ●──●──● feature/x  \
             \                  （PR 合并）
              ●──●──● feature/y
```

## 规则

1. **`main` 永远可发布**：任何时刻 `main` 上的代码都是经过验证、可随时发布的状态。
2. **新工作必须切分支**：从最新的 `main` 切出 `feature/<描述>` 分支（修复用 `fix/<描述>`，文档用 `docs/<描述>`）。
3. **小步提交**：分支上频繁提交，提交信息遵循 [Conventional Commits](https://www.conventionalcommits.org/zh-hans/v1.0.0/)（如 `feat: 添加区域截图`）。
4. **Pull Request 合并**：开发完成后推送分支，发起 PR；评审通过后合并到 `main`，合并后删除该分支。
5. **合并后立即部署/验证**：合并进 `main` 后应尽快部署或验证，保持主干健康。

## 日常命令

```bash
# 1. 同步主干并切功能分支
git switch main
git pull
git switch -c feature/my-feature

# 2. 开发并提交
git add -A
git commit -m "feat: 添加区域截图"

# 3. 推送并发起 PR
git push -u origin feature/my-feature

# 4. PR 合并后，清理本地分支
git switch main
git pull
git branch -d feature/my-feature
```

## 分支类型

| 分支 | 用途 | 生命周期 |
| --- | --- | --- |
| `main` | 唯一长期分支，可发布状态 | 永久 |
| `feature/*` | 新功能开发 | PR 合并后删除 |
| `fix/*` | 缺陷修复 | PR 合并后删除 |
| `docs/*` | 文档变更 | PR 合并后删除 |
| `hotfix/*` | 线上紧急修复 | 合并并发布后删除 |
