"""验收用的真实模型凭据约定（供应商 / 模型 / Actions secret 名）。

0.7.0 之前这条用的是 opencodego 的 deepseek-v4.1-flash。2026-10-21 发 0.7.1 时那个 key
过期了（HTTP 401「This API key has expired」），换成同一套 ClinePass 的 deepseek-v4.1-flash，
走 api.cline.bot 的公网端点——本机容器和 GitHub 托管的 macOS runner 都能到（本机的 magpie
网关在 192.168.0.248，runner 到不了，所以只看本机通不通不够）。

供应商 id、模型名和 secret 名只在这里写一次：验收脚本、probes、工作流、聚合校验和文档
都从这里取，免得下次换模型又散在七八处各改一遍。
"""

PROVIDER_ID = 'cline'
MODEL = 'deepseek/deepseek-v4.1-flash'
# `miyu ask --model` 要「供应商/模型」一整串；这个模型名自己带斜杠，别在别处拆。
TARGET = f'{PROVIDER_ID}/{MODEL}'
SECRET = 'MIYU_SMOKE_PROVIDER_CONFIG'
