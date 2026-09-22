## 索引 / 发布者变更自查(与 `scripts/validate-index.sh` 互为补充)

- [ ] `scripts/validate-index.sh` 通过(附输出)
- [ ] `generatedAt` ≥ 当前已发布索引(未倒退)
- [ ] `revokedKeys[]` 仅在追加,未删除或改写历史
- [ ] 未原地改写已发布版本的 `downloadUrl` / `sha256` / `signature`
- [ ] 新版本:`.ocplugin` 已由作者签名;`sha256` 为文件字节哈希;`minCoreVersion` 已评估
- [ ] 新 publisher:`keyId` / `publicKey` 与作者 `opencapx keygen` 输出一致;`verified` 决策有依据
- [ ] 撤销:受影响 `entries` 已同步移除或冻结;`at` / `reason` 已填
