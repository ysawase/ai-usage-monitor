# Muse (muse.ai) 競合取得方式の一次コード確認と採用設計（2026-11-02 JST）

状態: **RESEARCH_DONE / PERSONAL-LOCAL_DESIGN_READY / LIVE_PRIVATE_ACTION_HOLD / PUBLIC_STORE_HOLD**  
対象: 個人向け Muse（muse.ai）。**Muse Code CLI の quota とは別物**。  
配置: 開発用 Draft PR #10 / 実機のログイン済みWebView2プロファイルは変更しない。  
現在の本番: Rustモニターの既存providerとMicrosoft Store版は**無変更**。

## 0. 結論

- **UI Automationを標準取得経路として追加実装しない**。実Muse画面の `NoDocument` → `SettingsNotRecognized` → `MissingUsageSection` → `MissingPlan` を観測済み。ユーザーにZIP交換と画面操作を繰り返させない。
- **競合コードを技術的な第一候補に置く**。Mac版 CodexBar の `museai.js` とWindows版 Win-CodexBar の `rust/src/providers/museai/{mod.rs,model.rs,tests.rs}` が、Muse内のサブスクリプション情報をブラウザセッションで読み、weeklyのpercent/reset/planを扱う。
- ただし両者は **Museの公開された利用枠APIではなく、内部 Next.js server action `fetchSubscriptionAction` を利用**する。OSSのMITライセンスや競合製品の存在は、Meta側の第三者アクセス/再表示/Store配布許可を保証しない。
- 現在の実験WebView2のログイン保持は実機成功。**そのプロファイルをどう安全に認証へ利用するかは未実装**。現時点では実際のCookie読み取りもprivate actionアクセスもしない。Meta回答待ちは維持。
- 「個人ローカル検証」「self-only配布」「Public無料」「Public有料」は別々の判断。現時点で本番配布を許可したとはみなさない。

## 1. 比較した一次コード（2026-11-02調査）

### macOS: CodexBar

- `Sources/CodexBarCore/Resources/Plugins/museai.js`
  https://github.com/steipete/CodexBar/blob/main/Sources/CodexBarCore/Resources/Plugins/museai.js
- `docs/museai.md`
  https://github.com/steipete/CodexBar/blob/main/docs/museai.md
- `TestsPlugin/MuseAIPluginTests.swift`
  https://github.com/steipete/CodexBar/blob/main/TestsPlugin/MuseAIPluginTests.swift

観測: `browser-cookies`/manual session、signed-out判定、action IDの期限切れ判定、公開chunk経由の識別子再発見、成功時のみIDを保存、subscription情報の型付き変換。探索上限はpage 96 / settings 32 / batch 8 / text 8 MiB。sessionが切れた場合は `unavailable` であり0%へ偽装しない。

### Windows: Win-CodexBar

- `rust/src/providers/museai/mod.rs`
  https://github.com/nesszer/Win-CodexBar/blob/main/rust/src/providers/museai/mod.rs
- `rust/src/providers/museai/model.rs`
  https://github.com/nesszer/Win-CodexBar/blob/main/rust/src/providers/museai/model.rs
- `rust/src/providers/museai/tests.rs`
  https://github.com/nesszer/Win-CodexBar/blob/main/rust/src/providers/museai/tests.rs
- `docs/COOKIES.md`
  https://github.com/nesszer/Win-CodexBar/blob/main/docs/COOKIES.md

観測: Rust/reqwest実装。cookie取得源はブラウザまたは手動で登録したCookieヘッダー。Windowsではcookieの暗号化・App-Bound Encryptionの相違があり、自動ブラウザ横断読取は環境依存。固定action IDを使わず再発見。Win版は成功済みaction IDを**プロセスメモリ**に保持。HTTP redirect無効、要求タイムアウト30秒、レスポンス上限あり、cookie/生レスポンスをエラーに入れない。合成mock HTTPサーバーでFree/Power/Maximum、token/top-up、期限切れ/再発見/不正レスポンスを検証。

ライセンスは両リポジトリの現行`LICENSE`が**MIT**。コードを実際に取り込む際はライセンス・著作権表示を保持する。**MITはMetaサービスの利用許諾ではない**。

## 2. 既存PoCからの最短設計

ユーザー体験の確定要件: **インストール → アプリ内Museに一度だけサインイン → 同じアプリに利用率表示**。拡張機能のインストール、手動Cookie貼付、Chrome/Operaのセッション読取を標準UXにしない。

技術経路候補（未実装）:

1. 既存の隔離WebView2プロファイルでログイン継続。**Museのログイン状態を保持する既存処理を壊さない**。
2. 本人による明示的な利用状況取得を開始点とし、必要ならWebView2自身が管理する対象ドメインの認証セッションを*当該アプリのプロセスメモリ内*で利用する。OS全域のブラウザCookieは探索しない。WebView2の公式CookieManager APIは技術候補だが、**アクセス可否・Meta側許可を別途要確認**。認証情報をファイル・ログ・clipboardへ出さない。
3. Museの**内部**サブスクリプション取得経路を用いる方式は、実アカウント呼出し前にMetaの規約・回答を評価。採用できる場合だけ別の専用Rustアダプタへ実装。秘密情報の返却・転送面、認証主体を限定し、送信先は明示的に検証されたMuseのHTTPS originのみ。
4. UIに渡すのは `plan`、`weekly_used_percent`、`weekly_reset_at`（実レスポンスにある場合）、`updated_at`、`source` と `available/error` の分類のみ。subscriptionの生レスポンス、agreementの機微項目、Cookie、opaque IDをUI/log/snapshotへ渡さない。
5. 画面反映は本番provider採用・Store Gateを通過したあとにのみ行う。利用許可が未確定の間は **Draft/private研究のみ**。

### 取得/表示の意味（競合の実コードに基づく）

| 要素 | 競合のフィールド | 本プロジェクトでの扱い |
|---|---|---|
| 週間使用率 | `subscription.usage.percentUsed` | provider報告値。0–100%の範囲検証。成功/0と取得失敗を区別 |
| 週間リセット時刻 | `subscription.usage.resetsAt` | timestampが妥当なときだけJSTへ表示。画面に出た「11月6日」から時刻を捏造しない |
| プラン | `subscription.tier.name` | Free / Power / Maximum等の表示名。Muse Codeと混同しない |
| 残りトークン | 有料planの `usageRowValueLabel` 内テキスト | providerが明示した場合のみ。有料を仮定しない |
| 追加残高 | `topupBalance`, `topupTotal` | weeklyの使用率とは分離。単位不明時は架空token数を出さない |
| 有効期限・認証切れ | redirect / 401 / 403 / 404 等 | 契約消費0%に変換せず、認証切れ・仕様変更・通信失敗を区別 |

## 3. 採用前のリスクと決定境界

- 内部server actionは**非公開かつデプロイで識別子が変わる**。固定ID方式を採用しない。リトライは最大1回、探索数・サイズ・時間の上限を定義。HTTPヘッダ細工に依存するため、将来403等で止まり得る。
- Cookieはログイン資格そのもの。元ブラウザの保護を弱めない。**OS-wide cookie decrypt**、他の個人ブラウザ自動探索、手動Cookie貼付をデフォルトUXにしない。WebView2独立プロファイルからの明示許可付き用途以外は拒否。
- 既存のMetaへの問い合わせは2026-10-30のMuse UI送信完了が確認済み、現時点で **公式の公開利用枠API・DOM読取許可・private server action許可・配布条件は未回答**。問い合わせ記録で回答を更新。追加の質問を重複送信しない。
- https://platform.muse.ai/terms は**Connector Interfaceに関する規約**。今回の個人Muse用quota APIの許可証明ではない。 https://muse.ai/terms の個人サービス規約は別途確実に確認し、条件未確定を勝手にGOにしない。
- ライセンスMITはコード使用の条件。**非公開サービスアクセス許可の代用にはならない**。公開Free/有料/Storeへの統合は引き続きHOLD。

## 4. 次の安全な検証段階（実APIは呼ばない）

まず **offline mocked Rust adapter contract** を独立研究ブランチで検証する。実MuseのCredential、実セッションCookie、非公開endpointへのHTTP要求は使わない。

期待する最低テスト:

- 合成 `success=true` のFree・Power・Maximumを変換し、週次percentとreset timestampが一致
- 合成で0%/100%/小数率/不正型/欠落/範囲外の区別
- reset epoch→JSTの表示に日時を偽造しない
- 合成 `401/403/307` は認証切れ、404 action期限切れは再発見1回後にunavailable
- action ID変化・探索上限・通信タイムアウト・受信サイズ超過時には安全に `unavailable`
- cookie・response raw・会話・カード/個人情報をUIやログへ書き出さない
- Muse Code provider/既存本番provider/Storeバイナリに変更なし

**現Taskの終了条件:** 競合ソースの現物確認・UX最短設計・許諾Gateの明確化とDraft内への記録。mockアダプタ新規実装、実Museへのprivate actionアクセス、本番releaseはこの研究Taskだけでは実行許可しない。


## 5. 追補：第三者API経由の取得方式（2026-11-02）

**調査結果: 技術的な取得経路は存在するが、現方針では不採用。** 既存のPR #10/#11/#12、main、Store版への実装変更を指示するものではない。

- 非Metaの有料サービス **useapi.net** は、個人向けMuseを対象に `GET /v1/muse/accounts/{email}` を提供し、`quota.tier` / `percentUsed` / `resetsAt` / `quotaStatus` / `readAt` を返すと説明している。公開仕様：https://useapi.net/docs/api-muse-v1/get-muse-accounts-email
- そのAPIを使うには **月額15 USD** のuseapi.net契約が必要。Muse側の契約とは別の継続費用であり、年換算では **180 USD**（為替・税等除く）。公式料金：https://useapi.net/docs/subscription
- アカウント登録時に、Museの**メールアドレスとパスワードをuseapi.netへ提供**し、同社がログインセッションを管理すると明記。技術上は「パスワードを第三者へ預けない」「一度アプリ内でMuseにログインするだけで動作」の既定UX・セキュリティ境界を満たさない。https://useapi.net/docs/start-here/setup-muse
- useapi.netの商用提供実績は、**Metaが外部アプリによる第三者取得・再表示を許可した証拠ではない**。公開されたAPIがMeta公式でもない。Meta公式のMuse Connector Platform（https://platform.muse.ai/）は、外部サービスをMuseへ接続する方向の制度で、Museのquotaをモニターへ開示する公認APIではない。
- **現時点の判定: 現方針では見送り候補（正式なProject判断状態は変更しない）**。ユーザーが追加課金と第三者Credential預託の両方を明示的に許容し、かつprovider規約・配布条件の評価が成立した場合だけ再検討可能。
- 比較の起点となる隔離CIは、PR #11 `ff30028` とPR #12 `e8302e4` で**合成データの検証に成功**。実Muse quota取得の実証や公式許可ではない。保留を自動解除しない。

**維持する推奨経路:** 2026-10-30に送信済みのMeta本人向け問い合わせに対する回答、または公式の利用枠読取経路が公表された場合のみ取得方式の妥当性を再評価する。回答がないまま同じmock試験を反復しない。
