"""Pure source boundaries for the unreviewed Jenkins M27 reference port.

Historical migrations and unchanged production boundaries retain Owner26/cache4dc
bytes. The reviewed branch-check fixture correction is qualified inline below.
Selected reviewer files instead come from the explicitly included UNAPPROVED
2df4908 source context, not the admitted cache baseline. AST pins preserve the
unchanged legacy guards while actual functional tests exercise intended Resume
and coordinator changes. The task-organization context intentionally updates list admission, typed list
contracts and browser fixtures; those have separate unapproved pins. These are
archive-friendly source-identity checks, not approval, CI or admission authority.
The final combined candidate is based on actual merged cache main9bf5062;
source admission and consolidated CI remain Root-owned delivery gates.
"""

import ast
import hashlib
from pathlib import Path
import re
import shlex
import unittest


REPO_ROOT = Path(__file__).resolve().parents[3]
MIGRATIONS = REPO_ROOT / "deploy/mission-control/sql/migrations"
MANAGEMENT = REPO_ROOT / "deploy/mission-control/sql/management"
RUNNER = Path(__file__).resolve().parent

# SHA256 of the exact Git blob bytes, without newline or SQL normalization.
MIGRATION_SHA256 = {
    "0001_agent.sql": "62b639497347191b534ee2055bd4f09838ef98f69b0824c9bf2a55e5637b1331",
    "0002_ide_workspace.sql": "61fca7c93cfc2a38ffdf39f1c4ef189233676cc1e93e07faee50c1db488b61a8",
    "0003_ide_chat_history.sql": "91004ce176e3332dbb96c21d270564819994c2ccfe32d5ca7650bd509d7c0301",
    "0004_ide_chat_default_expiration.sql": "b060f253ba04dff86f1a1a3e338a102c7ce69c0e4b69071eb2a6a48aa60c9ea7",
    "0005_inference_dispatch.sql": "74285871ddfc316eafd5459d98c2364ae4172987461f1107dddea2948b84dcbb",
    "0006_inference_model_selection.sql": "b228402869abd7614e784c920225a442ba59d9fe0e6b4382206f39d987471ddc",
    "0007_ide_mail.sql": "58aeb69b6a0e760338378e5dc03c8606d55508265a32cad5e15bcd7cfe63f99a",
    "0008_mail_archive.sql": "755679fcd42d9c727be602406875b1419c5e7002f6af181595d8c409481b403a",
    "0009_coding_admission.sql": "4d9bf3aad61ecc281baf7a82b7e2e32b4d0414de3e138e8e8215b10d06c9f772",
    "0010_coding_execution_and_usage.sql": "a1fef2e563b83df633181040875c5c02b211a0dec89523aa5fc7cb87b7d0c6ac",
    "0011_coding_elevated_tool_policy.sql": "3bcd1a1cd440a57cd3d12b5d8116144079d5925c023debbdb880f3d0d8d9f0a2",
    "0012_coding_progress.sql": "f04db86c316e5fce931050d1325db117652e05a5fbd6d8a405e902a31c2515ef",
    "0013_coding_task_session_policy.sql": "2b6c8b3c25b5a5b91df0912db2d8dad37712a4395f9da290ea8fcb07a85a77ec",
    "0014_coding_plan_workflow.sql": "dcc61c328b7f3aa242c1fa7bc26a2c673c146f26cb6d4ee0f6f8125dfe1dd561",
    "0015_coding_feedback_loop.sql": "a9b35958de1614bd1bf04979152f674c85e6139372294df8b374ea68f60f06a4",
    "0016_coding_autonomous_workflows.sql": "90452fddd77f81983fb7c6eb45e96c2bcc85cea53937d5ac0ded6cee2a001604",
    "0017_coding_codex_warm_sessions.sql": "da3a0b0afef1cf1e823445fe7a13a07cf039efbc697f75f30a621d623c4463cb",
    "0018_coding_grok_provider.sql": "bebb2849e5e62a1e75e5d69943d9e1828574ca7d8c737c5eb42d8cc3de0936e2",
    "0019_coding_muse_provider.sql": "16012f62fb8769bc26f7305498976e37668809f0ddf52f6aa310313e5c464365",
    "0020_external_agent_transport.sql": "c68528d81371e5541b8843eb5a13e97f2bb190eba116575056ef824643fdd62a",
    "0021_coding_event_trace.sql": "27d0f242fde4e7f9ebb9f8960083de8fbc51d760728ca56510a6d6b50bcf08dc",
    "0022_external_workflows.sql": "04bb27131d4a7dc7bfaa8828a33105407deb6cc9c26441a96b543c5462f92f52",
    "0023_coding_agy_unattended.sql": "2282757e99d5c8b83656f7267b4aee5d269ad0130c30a89c3b89c9e716960fee",
    "0024_native_jenkins_context.sql": "ee78572dbd036a7c6330090b63017cb84cfc94e33d6a26b6efe18eb686519a98",
    "0025_native_delivery_repair.sql": "36b7ef798205514d6bcbdf2ca761de4aa8de476bee2be2b6ec3f97c720a165e7",
    "0026_coding_owner_continuation.sql": "676b4518d1e83768b3e9eb8bf440e1f5d60d338fd55928c3fc3858a141a3d007",
}

UNCHANGED_BASELINE_SHA256 = {
    "deploy/mission-control/sql/management/verify-owner-continuation.sql": "f5d7fcca9d4422e9e20a926149bcae0446b4ba598eab6f96400b199efc212c6e",
    "deploy/mission-control/sql/management/verify-feedback.sql": "7a260c86159801473ed09abf9af0abd399e39bd3222eb4d847475325a94c0834",
    "deploy/mission-control/sql/management/verify-native-delivery.sql": "d2e90311e17695fba72cc670b46038fb0e6528dddbc9921fb5140052474d8678",
    "deploy/mission-control/agent-runner/work_worker_store.py": "0fcfc4ac3f09a2c8e86ff98348e6898ebd77008f7ec07cf95b61a3757cb86499",
    "deploy/mission-control/agent-runner/work_publication_clients.py": "3d642c4d9112e00de6d203bc843613e0cbd3c6b32c21c1af235dbd810e01eb06",
    "deploy/mission-control/agent-runner/work_review.py": "70bb230dfd182fa740bba8cce8eee753fdad712aabeed34b1820443032a67d4c",
    "deploy/mission-control/agent-runner/work_native_jenkins.py": "ca8038ddb300fda22550dc028f52b5b026eff1898e7f9de6adb3d54848cc305b",
    "deploy/mission-control/agent-runner/work_native_repair.py": "7151e1e043220c6de1bf17889093268a8c9020f32b2d8f1b5a69550c301ccbc4",
    "deploy/mission-control/agent-runner/work_owner_reverification.py": "a2af7b2a293f9ebc6e6b8a1bd93546f522f496d0115754e58133a2d4f844746c",
    "deploy/mission-control/agent-runner/work_config.py": "6d342e8e50485947a3d8f436c0b81839e0ea72b967b47bad0d7c4657b9c0d1fc",
    "deploy/mission-control/agent-runner/work_policy.py": "38cbfda0443386cb10f78bc273c37496bb9469b37a7ebc55297e81aac77602ad",
    "deploy/mission-control/agent-runner/work_publication_config.py": "ff4fe40ba682138fb52865e25e6f6fbd0aed4b110cb6a88d95a81aa36b5f9533",
    "deploy/mission-control/agent-runner/work_external_delivery.py": "26fdc1ddec6c5e15e7368d46fff5e6f4f11ffd368e0da282a7a938ad07073f4c",
    "src/rust/forge-ide-ui/tests/work_review.rs": "bbc570e60b2532c2a4d7730505e26cd0470e738a50a37fda0491bc4983440d3f",
    "deploy/mission-control/agent-runner/test_work_owner_followup.py": "d791a3200da385177c3d2abd161be4bacfdac82eab2e98c3a26afd59a08bdce1",
    "deploy/mission-control/agent-runner/work_external_workflow.py": "29b94ec191e55565ad4455d8fa2b706a4cc1489fddc2a631715d94b0eafc8fd7",
    # Fixture-only branch execution correction reviewed in e5a1bd323a0182d845b87ec9303a408ce2612177.
    # Historical fixture SHA892ddd6dce467bd2924fc4aff352ef329c94013d84d578dfc460b040ee4d9b86
    # remains provenance; production helper, full gate and all authority AST pins are unchanged.
    "deploy/mission-control/agent-runner/test_ci_supervision.py": "0f468c483f4cb47d07191d7a07bba1682a323d570c9f7332f57ed406aa7ec1e5",
    "deploy/mission-control/agent-runner/test_work_branch_first.py": "0ba0f4ad5fbc1a73a6dc7b9d368c27ad68903b2c23169300db2c3f8145aa8c18",
    "deploy/mission-control/agent-runner/test_work_external_delivery.py": "3f077e13d41b4a6eadcd99dd9f6f62a90d621c9a964ae223de9c672224f90732",
    "deploy/mission-control/agent-runner/test_external_workflow_branch_scope.py": "1f876c2008fa9cfd5a284d7a3817af53c85ad226f98c383e2af780eeaaa27df1",
    "deploy/mission-control/agent-runner/test_external_flow.py": "807ac5b0c27e21b1cb069fd84d29a2d6a5f5c07dca040297fab96c5c89364ff9",
    "deploy/mission-control/agent-runner/test_work_native_jenkins.py": "4f954a6a79d9d40a8d2f5d50c5484afff085a4fafe183fbbd3817e94cef1df0c",
    "deploy/mission-control/jenkins/verify-candidate.py": "1ce4f51701da4cc12cb28cfc0c6ef00a4fb91b8eb9872dacc5890735b274af60",
    "deploy/mission-control/jenkins/clear-ci-cache.py": "b820fd1bd9e235e11a7e7cb511a8d57a141c4b67132558a930f8cca00fff9c23",
    "deploy/mission-control/jenkins/setup-ci.py": "e5aa3b67548f059fd863ba8ab45a309c3ab584eaead747072fbf149a762554b0",
    "docs/reports/forge-cargo-cache-qualification-20261121.md": "5a7c318495e114cc9c406ced1b32332799bafa08f96ccafc3921564f92adc54d",
    "docs/reports/forge-cargo-cache-qualification-20261121.json": "68e92dfb355ca2ae8ec807daebf9ab7102a4755d1ee3da2c283c28a9f3c90d01",
    "deploy/mission-control/jenkins/README.md": "7b9cef9d1e20c5174473fdf8474940863db13d56795eb40c94ff81af729de129",
    "docs/reports/forge-cargo-cache-publication-correctness-20261122.md": "31cf45a4a60f55fe209ff912204102e23f422cd446d7347c026616bb3ca29578",
    "docs/reports/forge-cargo-cache-publication-correctness-20261122.json": "3b7c8ae00b26973dc1295d90b45fa36c408d9c01ee7bdbcdecaf73f82962bd9e"
}

# Exact included context bytes; these files intentionally differ from4dc.
REVIEWER_CONTEXT_REVISION = "2df4908da25eb7a737678ce21cecc2ce0baf5428"
INCLUDED_REVIEWER_CONTEXT_SHA256 = {
    "work_owner_control.py": "16527f6a2217e766ed045bd7ecf66f35b4c647ff978064a135e1aeddcb30da10",
    "work_workflow.py": "1db7750216dd902674be195978dc11e1cac90864dad63d6498ff46f3a7b42cda",
    "work_reviewer_correction.py": "2b67452622e2cf924d0ce8970757823a7adaa9ef3d587e1828938e9d1807c80b",
    "work_peer_review.py": "56078e57c08f5e5211722feadcae2d6223cc657bc37972d7e94b319990b26de5",
    "work_executor.py": "149f8610fe150cb44816cc1daaf70261aab0a2acbbe46a85120cd9d69a4399ef",
    "test_work_peer_review.py": "eb9568cf1663401ecd9b7fbc1f69de23ed09c60e652d969c7ed95b2dcbe7a948",
    "test_work_reviewer_correction.py": "fd406873395e011afe393d4d7f03e17251c5cc7d79a29ee34b585744f8b5dcd8"
}

# Reviewed CI contract correction 7745032, integrated on 23 November 2026.
# Preserve the historical v1 baseline above as provenance. These exact reviewed
# source bytes add scope-aware v2 while preserving v1 receipts and full checks;
# regression tests cover producer/consumer agreement, policy/scope and skip fences.
# All historical SQL and legacy authority AST guards remain independently pinned.
# Source qualification does not grant deployment, CI success or merge authority.
REVIEWED_CI_CONTEXT_SHA256 = {
    "deploy/mission-control/agent-runner/work_review.py": "76edae6677e155bb7fcf470637a9c6b70c60d57b65003a922513354dca4acd5a",
    "deploy/mission-control/agent-runner/work_native_jenkins.py": "71e1c7ffc9c0ea4ae165aa199c844598f5094ac079b403a919c3e354c6073ec4",
    "deploy/mission-control/agent-runner/work_native_repair.py": "da4b1b1f423c588b567935eec7685b624a22addd51072a828904fc46546ee1dd",
    "deploy/mission-control/agent-runner/work_publication_config.py": "d937d9ade6a6f9df0e7d1ed023d1cce5606269731d81dc461983cb20f2f263b0",
    "deploy/mission-control/agent-runner/work_external_delivery.py": "f5319d992b195c91f1e811e4566f8ff8d90f60c7095458867211537452d130b4",
    "deploy/mission-control/agent-runner/test_ci_supervision.py": "847e2dadfcc02e7e1e47e0e867d90af5e3bd510e1a7cee4642ba803d55fcd1da",
    "deploy/mission-control/agent-runner/test_work_external_delivery.py": "ad65fdba13b676f7de35cd37b21b0bdb1d8d54ecc870f796125075fdc06e0d3d",
    "deploy/mission-control/agent-runner/test_work_native_jenkins.py": "98996c669a8fa2618c2de15cc67e995285bf0a649d20f730efe899f42886d23d",
    "deploy/mission-control/jenkins/verify-candidate.py": "0a998d4148cadf6522c1dea6523dfaf74644a765a447cfaad27dc8bcff212725",
    "deploy/mission-control/jenkins/setup-ci.py": "5b6ba065820fb7528ffb3abe46e14960d8a0b304d34f22ca0a0ae769c507eae3",
    "deploy/mission-control/jenkins/README.md": "667696432a4321a8e4a929cb2b76844f50eb4f02dfd5ac2f356afe2c980efb74",
    "deploy/mission-control/sql/management/verify-native-delivery.sql": "5446f8f6adc7a764069a3f5476546ae8e78649de02510fb494c64b6b4f5375de",
}

# Python3.12 AST serialization; no Git or runtime imports are needed.
UNCHANGED_LEGACY_GUARD_AST_SHA256 = {
    "work_workflow.py::BoundDelegatedAuthority.validate": "3c5684800e9fd927d175c1b184368e3fbe9b47f88011fbf01704ac42125c9ac0",
    "work_workflow.py::WorkflowStore._code_handoff": "28c134dec07214188396c681e59bf90bf81c8592bd645019790ba11977916b02",
    "work_owner_control.py::owner_continuation_used": "9360ba6cabf3fd2100c0baad30f40a0e7ca28206dcefbe8edfaf4279d0fb6ec8",
    "work_owner_control.py::_owner_source_history": "2e944f3a0bdd32d24c1130180be4293f876f7cc6588ef2cdb9eb60635288c1da",
    "work_owner_control.py::_followup_replay": "91b40027cd2166441c903826b5c436cc025d348097d7d9eed2508b671bb2776f",
    "work_owner_control.py::require_owner_capability": "4d69560a3daf28ac3903d962ba6e9f11ec317a8c1983b63fbf54018600471c68",
    "work_owner_control.py::NativeOwnerControl.review_decide": "9931f291b49bc0801c928fec5897828243ade1e229bd890cf54a7d7f503641e4",
    "work_store.py::WorkStore.plan_decide": "9fef752f5c30754bdd228f378c827a0547dbef840ec09552f56c64644fd4d5a7",
    "work_store.py::WorkStore.review_decide": "7984ccd6a5146b99982c239cd72b14ffbca8f275105cff3e79788d65c62e45a2",
    "work_store.py::WorkStore._append_review_retry": "cd87596f0deed82998e5e1c38d535317c58eb629481a7a7285b5c794bb3a4355",
    "work_store.py::WorkStore._publication_busy": "8e56623d967373002fa0b9fdc6634ca2741097296e965fa4b27cc09856e06ea4",
    "work_store.py::WorkStore._require_transition_authority": "ef19c73adb7a69244d38a747189357b26cf74f7c3333f921c5985867be9c416c",
    "work_workflow.py::BoundNativeRetryAuthority": "e24dd6666fbcce44177b849f6048b810a87f93f5ee99a4adea537fa0a8bcc604",
    "work_workflow.py::BoundPreparationRetryAuthority": "e2c4dd1dda211ccdc02b743548c75db544423dd325c3d925a1e556acb2ec3c81",
    "work_workflow.py::WorkflowStore.retry_native_timeout": "a1e99b03522a9cebb5a8833505bad2fd0546d039f5c7404f1b6ba88119aa816e",
    "work_workflow.py::WorkflowStore.retry_peer_verdict": "3c194b82651b8a355daad066e579ecddd231bf520b63ef68450476ad6396ad87",
    "work_workflow.py::WorkflowStore.retry_preparation": "a682c06026a0de95877b70d010bb605e04afc086934a1c1b4e63f656c58a8e57",
    "work_workflow.py::WorkflowStore._code_review_source": "3679116c308d0af3517344bf053f58ed720c054a4e8c1ca6bc08e028932e0783",
    "work_workflow.py::WorkflowStore._exhausted_code_budget": "6079ee6516a533ce869d6716a140d8dfbd3d64861e3ee0c12026c35aec296e81",
    "work_peer_review.py::parse_review_verdict": "4f34e864c6a94883122fcd86d55439e4522adfd525e9dde334fb9e5c7852e83f"
}

# Explicit task-organization source qualification, based on combined351e.
# These selected files intentionally differ from both4dc and reviewer2df.
TASK_ORGANIZATION_CONTEXT_SHA256 = {
    "src/rust/forge-ide-web-host/tests/consumer/work.rs": "93f109ca3156fa187331af2367355ddf7d2ff474817d515ea5213c7fbb5504f2",
    "deploy/mission-control/agent-runner/work_api.py": "88d946828d34ba97d982c9e23b6ea36d2db387358f72d3f422c27246ef7325c0",
    "deploy/mission-control/agent-runner/work_store.py": "07a36e7f9a90edf895dd260cfc1f27578c20c2af0e03ab4519c9ee2eccd8eb05",
    "src/rust/forge-ide-ui/src/protocol/work.rs": "8cf09a3108b9e67cb8b639f43aaf9a903b7140494fec363e76edb9576b1aa8da",
    "src/rust/forge-ide-web/tests/browser/agents-relay.mjs": "7516de55bd39f341a2b440dd76593ac89659f41a40a064b9a7f3d7908c37ac2b",
    "src/rust/forge-ide-web/tests/browser/agents-relay.test.mjs": "6603adbcc3e73f4cfab45f984c663285b4630502e51a6004438828fb5d688312"
}

# Retirement feature origin is retained below. Its source fingerprints are
# qualified against main370bcffd6d4c71d83ba0fa8742257713da8cf76d after the
# reviewed module formalization, forge-core imports and additive contract tests.
# Those structural changes preserve dormant Chat/Mail data and retired routing.
# Historical migrations and authority AST guards remain independently pinned.
# These fingerprints qualify source only; they confer no CI or merge authority.
RETIREMENT_CONTEXT_REVISION = "243742283dc1a182738c2d4ecf131618b1dc7b8e"
INCLUDED_RETIREMENT_CONTEXT_SHA256 = {
    "src/rust/forge-ide-ui/src/lib.rs": "251d2bdde5ad1fb385f36fde134edb07b3832f448375e6405bdfcb5e79488b34",
    "src/rust/forge-ide-ui/src/model/bookmark.rs": "429b2b860a19c1402c08e6eae7b2f63413b6fef4db9a602c7617c1f8a5efa496",
    "src/rust/forge-ide-ui/src/model/edit.rs": "e86329d9bacbcae64dd0001fb48a1cc66f12c28e7e585d85309163c27f1e2d7e",
    "src/rust/forge-ide-ui/src/model/projection.rs": "a45f8dcad634b1723d95df341cd4f569a999d8ae9c4c66145f88f0d3ef653597",
    "src/rust/forge-ide-ui/src/model/source.rs": "ab667c5794ef6b64ef92161cc349583feb17f3e344253353f28075500fda533f",
    "src/rust/forge-ide-ui/src/protocol/legacy.rs": "41166392429986311d1523e05dc9a5869732d1268eaa4a477aced8d956402c32",
    "src/rust/forge-ide-ui/src/view.rs": "8ccd4caf02eec07539a3c13199908a6378ea730de305d851aaa3cdb934e97517",
    "src/rust/forge-ide-ui/tests/chat_retirement.rs": "de5d96a3423cc4e101ab086bfde48b239a132fdae495de3f3b9091028335d44c",
    "src/rust/forge-ide-ui/tests/mail_archive.rs": "9f712228d7e3c6528fb821402c0c94e8e0c2485f7240be73ac87e1510fb18514",
    "src/rust/forge-ide-ui/tests/model.rs": "b433412ef0655aabce964d49622ba0edec99907dc434c7373e0c96c0def2f474",
    "src/rust/forge-ide-ui/tests/wasm_abi.mjs": "33f15dcfd90c02d2959a9e3fb226d5d9e66e4582d77521dd9dc91ba91f7c38fe",
    "src/rust/forge-ide-ui/tests/worker_glue.mjs": "4ebf78598e8808f0d30ff69645be729ece0d6d94bf7f785f6ec28365f221e71c",
    # Reviewed active-guide correction in 223ca465: ABI13, inactive Chat/Mail,
    # current live/source distinctions. Retirement behavior and authority stay pinned.
    "src/rust/forge-ide-web-host/README.md": "97c201507af30afb509c13673abfc1db35d3a339eedc89e404cb0d65eb0c610e",
    "src/rust/forge-ide-web-host/src/assets.rs": "28fce4e5d3f74d4b50e979595521bf22a0ac666eb8f2892d43b6b66966ca717f",
    "src/rust/forge-ide-web-host/src/chat.rs": "1cdf9da99a655a2c6112e35125d97f86ee161f5fcb013a5f738eb75abde67756",
    "src/rust/forge-ide-web-host/src/http.rs": "7d0fdae9a170186a4b3fe482d645d18194ef64496b05021fbd2ce778d56328f2",
    "src/rust/forge-ide-web-host/src/lib.rs": "94cb9ea44720babe100348ebf28bde0357a96ff0590abcb7dd298db249492a25",
    "src/rust/forge-ide-web-host/src/service.rs": "bb683078c9c4fb6e5ac7961e27af608c0bb470db2b7522d21071887c900b1ae8",
    "src/rust/forge-ide-web-host/src/service/edit_write.rs": "ecceaf207765cc76061852cb7aa76208f8de1cfe3db38299a4f04bd18829450c",
    "src/rust/forge-ide-web-host/src/session.rs": "492341fc9132ef2d0bed973f08c2cef8853f0c9dfb4b19b1630a71f2ab4da2cf",
    "src/rust/forge-ide-web-host/src/workspace.rs": "f3ee8f4c5b59ed8d5702d96f880235e34c248324cd2541566fb346200dfaf606",
    "src/rust/forge-ide-web-host/tests/consumer.rs": "fdda3df4e4941d055b87d078881c4582b5b603857d7805df50ba05d1cf159bec",
    "src/rust/forge-ide-web/README.md": "d57c1f04589a5e83d9de0fa4cf22f21759eb258253dfd831d1e6f55209748732",
    "src/rust/forge-ide-web/src/generate/javascript.rs": "e3efa60edc38d95b51aa30c2608059c997ccfceeb6656174788e58c079b08622",
    "src/rust/forge-ide-web/src/generate/mod.rs": "77a6552cb4017cac4184b9e4a211c8e77892a3dc568889d212553f81af6e93a7",
    "src/rust/forge-ide-web/tests/generation.rs": "a22651fe5b5d49b25ffad267016b6c839cfed04a6c1393e4ac7f4655ad4dfa23",
    "src/rust/forge-ide-web/tests/playtest-wire.mjs": "44c28f99689fc3147af455c5357f1876792c3405ea24ae65dad8d7062ea50937"
}

CONSOLIDATED_UI_CONTEXT_SHA256 = {
    "src/rust/forge-ide-ui/src/model.rs": "bf9a80504a7902811bbe05aa1c218e36777f7a401a116f28709b38a524f79b33",
    "src/rust/forge-ide-ui/src/model/agents.rs": "b8fd5f7624b00c51b124b767060b7b1301dabeed8e57a95403d1184823420a1a",
    "src/rust/forge-ide-ui/src/model/agents/tests.rs": "8ef1a21ffb94b03463b7969dfd6d8f7c5d4c17a3e9bc01631e7e8fb925156f74",
    "src/rust/forge-ide-ui/src/protocol.rs": "bdad229225ef916154a747f29516286e21875cd6855acdf728bd60ec03fc1f5f",
    "src/rust/forge-ide-web/src/generate/application.rs": "b9d29c96611fcf87aa5022367428135657dd7886039a8b71c900fdb8af0acd85",
    "src/rust/forge-ide-web/src/wasm.rs": "1f97a8974f37d3d82a444a8b614db38cd6a27f9a23e2674ed0c80e8fc74ab5ee",
    # Reviewed 73315d3 + 239ec2c align assertions with the current implementation
    # document; prior retirement-context fingerprint remains provenance:
    # 54fb3215c4698477193b5456fd2af5e0d215225f9023a44ee0a1195a9512f402.
    "src/rust/forge-ide-web/tests/browser/journey.mjs": "b66170861cca572fd8b3a9f47740cb474427614bc572356a521bedd3749e7e3a",
    "src/rust/forge-ide-web/ui/styles.yaml": "b7fb1b60e30d2afff4e3eadd7c45ef474ffe2f66f0d95b27b7668ad57464188c"
}

RETIRED_BROWSER_SOURCE_FILES = ('src/rust/forge-ide-ui/src/model/chat.rs', 'src/rust/forge-ide-ui/src/model/chat/history.rs', 'src/rust/forge-ide-ui/src/model/chat/models.rs', 'src/rust/forge-ide-ui/src/model/chat/run.rs', 'src/rust/forge-ide-ui/src/model/mail.rs', 'src/rust/forge-ide-ui/src/presentation.rs', 'src/rust/forge-ide-ui/src/protocol/chat_run.rs', 'src/rust/forge-ide-ui/src/protocol/mail.rs', 'src/rust/forge-ide-web-host/src/edit.rs', 'src/rust/forge-ide-web-host/src/history.rs', 'src/rust/forge-ide-web-host/src/mail_dispatch.rs', 'src/rust/forge-ide-web-host/src/run_dispatch.rs')

# Reviewed additive M28 coordinator context on main aae81e4. Historical raw
# maps below remain literal provenance; only these explicit current paths change.
# Functional/privilege tests and unchanged legacy AST guards remain mandatory.
POST_MERGE_CONTEXT_SHA256 = {
    "deploy/mission-control/agent-runner/test_work_post_merge.py": "39b7fd47aeb478cbf11909f546f3fcd81a668cbd6a26999eed5b038cc4ec207c",
    "deploy/mission-control/agent-runner/test_work_post_merge_provider.py": "12939c18f7f390aa1e5aec240cafcca79a56ca07d87f2f73c8582f1ed5215bb2",
    "deploy/mission-control/agent-runner/test_work_post_merge_store.py": "e176837ae4bf9ea317c71f65500f28d321abaa5371b31ee6ac87272a51848858",
    "deploy/mission-control/agent-runner/work_post_merge.py": "e60b97397de161ffbee37d42a28f17b8feb45feae9bcf2f005eea7474cffdb16",
    "deploy/mission-control/agent-runner/work_post_merge_provider.py": "a4959d8a5462c1eaaa9d8f62e45419f08550ab8c91c4cbb60890f03949d706cd",
    "deploy/mission-control/agent-runner/work_publication_config.py": "03ebdeff74a4943962a397af47a8201dd45b0592138f5901db2df74b151db8c9",
    "deploy/mission-control/agent-runner/work_worker_store.py": "c52f5c9267d3eb3152fc007589b8a7963a312b61b440174536661f6d7cf3f3a8",
    "deploy/mission-control/jenkins/README.md": "39ac4d75f6c73fb952157e0fd620a1c88f70c162211b5bcc51e9f331168c2008",
    "deploy/mission-control/jenkins/verify-candidate.py": "6d17f6da2fb6b4d8db5481d9d70e6c089c21b994f24df3728581b2480e36827d",
    "deploy/mission-control/sql/management/verify-post-merge.sql": "e89742a93465c37c786c09aec7fec9ac00cd250f34e02d4a400f44e518272165",
    "deploy/mission-control/sql/migrations/0028_post_merge_verification.sql": "dba349627f5583fbb37956d0742b521e7fb257879efbbe2d61389d792cbe4a00"
}

# Reviewed ABI14 delivery projection in f246b1494f29cf86af8a32a24da80d56c17427d1.
# Keep all earlier retirement/task-organization/UI hashes as literal provenance.
# This narrow current overlay binds the negotiated DTO/header, typed rendering,
# atomic ABI metadata and their tests/docs. It grants no live activation or CI
# success and changes no SQL, native publication guard or backend source pin.
UI_DELIVERY_CONTEXT_REVISION = "f246b1494f29cf86af8a32a24da80d56c17427d1"
UI_DELIVERY_CONTEXT_SHA256 = {
    ".github/workflows/ide-ui-browser.yml": "f4e6b1b31e45bcb639025624f0407e615db41eae87ba86edf3c562a9576727d9",
    "docs/architecture/forge-post-merge-verification.md": "8996a8731a6444c16903f66ac5e92e7378aa2d968e6970ee9329e4d39b1d5d2a",
    "docs/design/implementation-status.md": "c1eb6ed9a29082de176a5d6c5276de5d6e74bfd87d9cfb6711cbd7597d873939",
    "src/rust/forge-ide-ui/README.md": "96e4edc242943139c8d5d3d8732c33d86d42e797801586397cd93bab87926043",
    "src/rust/forge-ide-ui/src/model/agents.rs": "141d8a545fd1b5c2ed09ce12cb7d6955f3669b6f697e8a85d79b9667c6616b20",
    "src/rust/forge-ide-ui/src/model/agents/delivery.rs": "309cc497753fa389cfd560ecadfcf734e58ce27cdc39caad412b4dc7d62dca6a",
    "src/rust/forge-ide-ui/src/model/agents/tests.rs": "d6af0cf1803cb303b9df65ab61d5a57cea1783308fa1f1a1041bf9091ad8ec8c",
    "src/rust/forge-ide-ui/src/model/agents/tests/delivery.rs": "76d33b51a8fc72b4ffc4ec6efec99f64390de871e85b5e50a6f0def183598f8f",
    "src/rust/forge-ide-ui/src/model/agents/workflow.rs": "d32eb1fac1d684c9e9be6770e4efa606bd26ab7a9913e008e048aab7a4f98ee5",
    "src/rust/forge-ide-ui/src/protocol/work.rs": "1ba9c548db04fcea2098d092e2a323535f3b151e2ec508915304abf3f5f358b9",
    "src/rust/forge-ide-ui/src/protocol/work/delivery.rs": "27a4500fc1413995ff0db2ee1b850979f10e9c171edb232a5934afd9aa1ca16b",
    "src/rust/forge-ide-ui/tests/mail_archive.rs": "f3248838fa6a715a7b2da95423748f40e5463da661868b0002fe8432253207c4",
    "src/rust/forge-ide-ui/tests/wasm_abi.mjs": "42b0be15b84b6874fedf773098a7e4d74cfaf934199d42075a28598a7162c388",
    "src/rust/forge-ide-web-host/README.md": "28e8e54458d6683a8b653ca1ac63c4378c2ac4d4e6b3cb990a7bd0d7d649a8e2",
    "src/rust/forge-ide-web-host/deploy/README.md": "86365686bfdf9368a3facb6aef771e8fd60c3da8dfe41701de088ed1884bb811",
    "src/rust/forge-ide-web-host/src/assets.rs": "61dbc342027700a1ca40cd34c2d30a61c99a0c6609f314ab6691c9f194b1fed1",
    "src/rust/forge-ide-web-host/src/chat.rs": "a16649ddba509aa7f20f902405914b98404553d2d983ec58493e720c60e60bc5",
    "src/rust/forge-ide-web-host/src/design/agent_autonomy.rs": "ebcb01e1d0c091c386737f7687d835580e2d318d3ac88fecab566961789977c6",
    "src/rust/forge-ide-web-host/tests/consumer.rs": "0746ca8a66b971dc73fccb11748310ea6831eb7740317af16ec1878fc9501ea0",
    "src/rust/forge-ide-web-host/tests/consumer/work.rs": "3eae52a847a46afe4638a650e5c44d02cfbecb9a85992d3b0cbe414860421156",
    "src/rust/forge-ide-web/README.md": "54c8fd89b9d2da3e4b6d406e0eb561f7204f302cdfb78103497a054cebab8f24",
    "src/rust/forge-ide-web/WORKBENCH.md": "eb33ed9814f1e4fdf13ba919e251cb046219fc9a494233ff922b00500d2d54d6",
    "src/rust/forge-ide-web/src/generate/application.rs": "550b19bf64d32169d4ae906f5ca7136aefa0a2e260c8feb45ad3180ecbf22de9",
    "src/rust/forge-ide-web/src/generate/application/delivery.rs": "2e466be23b1fa436c6d3094a53742eedc74983567385f90b3f99bfc2a01355fb",
    "src/rust/forge-ide-web/src/generate/javascript.rs": "f4cb2f0a59be217c7c6b961b556fad71be3d4ef889d3dbc1dc35ea92de4bb44d",
    "src/rust/forge-ide-web/src/generate/mod.rs": "d1e03f840fcf45e71a3469497643c620f1f6cdf30d2c81c071e1a19ae8c00af5",
    "src/rust/forge-ide-web/src/wasm.rs": "bf857b74aabc157492665e3b8081bc9c11e1965dd21c43578f34fa53f102c2d3",
    "src/rust/forge-ide-web/tests/browser/agents-relay.mjs": "19c7ef943a10590661807770b4428d1d0cc52960109c12c999a15f909a1d46a7",
    "src/rust/forge-ide-web/tests/browser/agents-relay.test.mjs": "d0cf35feb49a7b100799ad4fd9e4271161442e6e1ed46942b7e09dd168db583a",
    # Browser-only capture follow-up: close exact-source details before screenshots.
    # Preserve the f246 journey fingerprint as provenance: 56602f5677b962c3440e3d4447206dca790ec9a39ba3f35d56d0d2a366a30bc2.
    "src/rust/forge-ide-web/tests/browser/journey.mjs": "7c130f5335cc511ba9d3e2c445055fe210af07c84ce81979159c4bf47260fb34",
    "src/rust/forge-ide-web/tests/generation.rs": "03c30669e72497842be36d6e638e232205e6c46c85d03c61e976d4a851f72e9d",
    "src/rust/forge-ide-web/tests/playtest-wire.mjs": "4528a669e7fee2dca2c1fd099364793075cb3671de9ced507475c7f3cd1031bb",
    "src/rust/forge-ide-web/ui/styles.yaml": "960473ec8185275bcf0901e48d0bf2fad48c27cfa05bc056c479b2705277a9a1",
}


# Current additive delivery source; historical maps above remain provenance.
DELIVERY_AUTOMATION_CONTEXT_SHA256 = {
    'deploy/mission-control/agent-runner/runner.py': '4e112cd0624e46c8964e37e66c86eb274ce7392cb02a1dd24b548fc8a075537c',
    'deploy/mission-control/agent-runner/setup-runner.sh': 'a346e8e4fd5e64b47b4a40419f2e9195e7097e752340e486399054e63faf36b4',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py': 'de459b8d5a49922d5272c186f887bad40afb87421661d31fc0bfa85ac85e0664',
    'deploy/mission-control/agent-runner/test_ci_supervision.py': '8769a4232b38c14932917099c6f30ed7a6b22d725d29f412d48efcecf991cf3a',
    'deploy/mission-control/agent-runner/test_delivery_ops.py': 'd0d9428dfe0e264a7dce8f2edf9e2562eadbf29bb425adceae18040c39cf4a7a',
    'deploy/mission-control/agent-runner/test_delivery_worker.py': '07ce04244ce3fc996b0536ac7140dd698877df8e08258d269e79aa9199b3b2f2',
    'deploy/mission-control/agent-runner/test_muse_read_access.py': 'fa9dac349452485440260861f6ddf499ccdec11892a4eb5b992cd67232a1b9a1',
    'deploy/mission-control/agent-runner/test_portal_release_actor.py': '3e82a752626030b109a7703d80942dda81f947e0d75782125b7a2b95f965a324',
    'deploy/mission-control/agent-runner/test_portal_release_recovery.py': '83fdbff03f32d9403d055ade2fe90530a591dbe0df4cbe8a0e6fd61782b40249',
    'deploy/mission-control/agent-runner/test_portal_release_startup.py': 'e71343b033f13f443393ee605be95a6346b7b43481e502fd34b1ba65b1c50b9f',
    'deploy/mission-control/agent-runner/test_work_context_recovery.py': 'c8d92523ee270f8e664c46b7c23358efca91005cc32f15352fb3aca9ae0a905c',
    'deploy/mission-control/agent-runner/test_work_delivery_automation.py': '5b6bc79250f91e85eab447eb358eb3107b6a6e1a73811055ad79f4965ec95c26',
    'deploy/mission-control/agent-runner/test_work_feedback_delivery.py': '6cabb6e884baf3d8172c7c5b8eff3351cead8a807ee3a6e1a51333d632092541',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py': '94a79619048c5c8fbe599db9ae8f90782884da1652e877f5bcc9f2eeb8cac9c2',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py': '86b555fa3622264fd00e2e14fa1640ccbcf8a215e3d388f7243954fdf7fb38e1',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py': '37667ad1681094f52d5f777317da0931b606bb1bda44b2393a2ab4e7631efcae',
    'deploy/mission-control/agent-runner/test_work_post_merge_store.py': '74ed358df596751c8a34d539bb3c8fef3152d9dc369a2cb1a437b614f5aa1928',
    'deploy/mission-control/agent-runner/work_api.py': 'a3707fae102279bbd2f7abaedc26bf43c0b2f543ab180f1b97cd96e164800898',
    'deploy/mission-control/agent-runner/work_ci_contract.py': '71b55c469064b394ab16aaae9ab336634eb3549417da0e1064dc015c3e59c37d',
    'deploy/mission-control/agent-runner/work_config.py': '1d67059a436ebe3b9bd4c3c470bb43cd0097d161fcf4fbd3a70f1f41e1b53172',
    'deploy/mission-control/agent-runner/work_context_recovery.py': '440d4cfb013ccdb0f028554c7978606a22c3cae3ad5c6500fc80adbb41c26e5c',
    'deploy/mission-control/agent-runner/work_context_recovery_contract.py': 'ecda9366299a734492b3837e1f1324e156f5da8eb2256266c18937c7da473071',
    'deploy/mission-control/agent-runner/work_delivery_ops.py': '05fcf5728e9ebbc0cd58a206e0345e24a24ad3151995d351613fc50fffd648da',
    'deploy/mission-control/agent-runner/work_delivery_repair.py': '3d06ea4e5edf6036b31187087c5c48b83d0fcbf021b3d9e259d39e0ce0150c6c',
    'deploy/mission-control/agent-runner/work_delivery_worker.py': 'bc2fed4f80b80a3c96b02b3d6a1f6b9cb0fb88fa520834efc142711920626ad3',
    'deploy/mission-control/agent-runner/work_publication.py': '035eb4d919918e80bdaa8b4c5242e069fc3e17036ff84f6a1bd46a186f356bf0',
    'deploy/mission-control/agent-runner/work_publication_config.py': 'f9f235ca112aefe2d6214e6c26ee4e74541a2ae2b21f96875ce17b37bd907a0c',
    'deploy/mission-control/agent-runner/work_review_context.py': '80e855c546c1164cd771c5cd7af34642f6f1640c8fdf9f957423c9a974d82079',
    'deploy/mission-control/agent-runner/work_store.py': 'de047f8f6ef3dcf5fb1f3752b9efaee99a00956bf4817fd6cb0f5346c2b0dffe',
    'deploy/mission-control/agent-runner/work_worker.py': '20af22323b743855315f7d431da1aefe2368e6236dfaa06420e14f014de51fe0',
    'deploy/mission-control/agent-runner/work_worker_store.py': 'd3120c4b9cb3e234c247b5e07bb6d1e8c2dd5e0c7acbe12be88ae1868f28ad05',
    'deploy/mission-control/agent-runner/work_workflow.py': '3555104b630ad6d4f3caaa290f1e29bb33e9d221438d34ab928ca93d17a69fcc',
    'deploy/mission-control/backend-suites.py': 'b181b48acc588f76542d6f60fa98e1700ef9514bb7904c8936aa74c9d2d5709b',
    'deploy/mission-control/jenkins/setup-ci.py': '8d8b6fdf59d24d65dc0983ddace6d0d5a89418007d4338228dfdb695a3e05170',
    'deploy/mission-control/jenkins/verify-candidate.py': 'ed8372fc251741505c2f664ea3fbf5dc5bed72c6588b8c93e08af814cbd0a1c3',
    'deploy/mission-control/portal_release_actor.py': 'd97cbc08864c42595a615606ecf3bb543aeedd88c614a22202b476e7eb6d5fc2',
    'deploy/mission-control/sql/apply.sql': '83471649c4b88c381c4f449135f28675b962fe9706caec4842598fc46a7dfb8d',
    'deploy/mission-control/sql/management/grant-muse-read-access.sql': 'ee52fd223d7edd1c2691f320da508a2f31dbe79d3678579521a0ea9154b83523',
    'deploy/mission-control/sql/management/grant-runner.sql': 'cf99de3c8d2d18115591143e1f9142eb34fc35fae1ca7d2f98be838671779ed8',
    'deploy/mission-control/sql/management/verify-delivery.sql': '3b3e183357db78fec0e4ae5f7200ca227ffe771dd449536b4eb0d9a7315f5f15',
    'deploy/mission-control/sql/management/verify-muse-read-access.sql': 'c351265a894c8bc04abead41a4902ead8b03a679f562e8a8932e836407449f09',
    'deploy/mission-control/sql/management/verify-post-merge.sql': '7bc90aea2d4b4c567d46ae89e85acc6772483d36c0b9af34ba6de1b10b20bc01',
    'deploy/mission-control/sql/management/verify.sql': '71674f4b392b34a088a5aae86f36fdb976190db4ecbd67430b6c6d6eca27de1e',
    'deploy/mission-control/sql/migrations/0029_delivery_automation.sql': 'e4ad1045a4cd0fdaf1d2f2eff2fbe639e7f1ace7005c29eae9a7f1ff92aa8926',
    'docs/architecture/forge-post-merge-verification.md': '26cc6af972517f89fba5dc6d225576827ed9755b34dfb28dd1593b335e8ac640',
    'docs/design/implementation-status.md': '381c951d14f8e74c1e47ef658081a2743f61a74b2d43b1eccf53da53644679d1',
    'src/rust/forge-ide-ui/src/model/agents/tests/delivery.rs': '0caceda197b712f2fa8d24a76f2a4315e9ed984fe8836f2470854624154b04c8',
    'src/rust/forge-ide-ui/src/protocol/work/delivery.rs': '1550d7700495aa477f5f843e4e0e9cdcde59be4fd11f22fa2c749212dc54ea03',
    'src/rust/forge-ide-web-host/tests/consumer/work.rs': '26505887977ab190cc9c964a04a733b4c48a4600bb03c4effa9f9bfbbe894ae9',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs': 'd147469ba5418ba0e8340e7ef93b080a6590cc7c84ac1ca95d0859190a45a19a',
    'src/rust/forge-ide-web/tests/browser/agents-relay.test.mjs': '37d11e92a01395710fa095502198aef60001da79eb7fa61dd44286941acbd150',
    'src/rust/forge-ide-web/tests/browser/journey.mjs': '1b40792d445944e752effdb442b722a05955391cc6a06c100e1e7f9f746198a6',
}


# Reviewed agent-wide retry overlay. Historical source and authority pins above
# remain immutable provenance. Only numeric allowance reads change in the three
# guarded methods below; roles, grants, recipes, source and replay checks remain.
# These fingerprints qualify source; they confer no CI or deployment authority.
AGENT_WIDE_RETRY_CONTEXT_SHA256 = {
    "src/rust/forge-ide-ui/tests/work_review.rs": "0b3bfed9322842760840823fed6589cbf1875b4f3b42cd2c01c9135d8a0d01c1",
    "deploy/mission-control/agent-runner/setup-runner.sh": "c25bc0c27040d513095c1cdeb31b80748548ba3c3384eef06070d52f40757a30",
    "deploy/mission-control/agent-runner/test_external_flow.py": "b6fc29b6ef427b06656104b5426bce8f71b1a01fc64f345c8e1c7e30148febdb",
    "deploy/mission-control/agent-runner/test_work_owner_followup.py": "369b9d91e2098e7c42f340f905aa449c2e14f559782f8579eab80f3d55335be2",
    "deploy/mission-control/agent-runner/test_work_peer_review.py": "9cdddc629df8806cf647ec9f70a71643e31bc461c8c25e7819fd267747f3ebaa",
    "deploy/mission-control/agent-runner/test_work_reviewer_correction.py": "b607a99b103dd7232edb0d4b78c189deaed30b9d7217f7a8d5d9d218c250905b",
    "deploy/mission-control/agent-runner/work_config.py": "d70cf089b24d90383504e97aaf2200361a221704f043df6c7ce0d5eab7ba034b",
    "deploy/mission-control/agent-runner/work_context_recovery.py": "fa7d2f55425659975d9c05c8f85264ec87f6095f3129b5fbecd53bcc3c870f60",
    "deploy/mission-control/agent-runner/work_external_workflow.py": "4b67f29e2642a4c2c4420561b746e4b98d75acffecbabbee19426c441e79e97c",
    "deploy/mission-control/agent-runner/work_owner_control.py": "572f23e141e29e7a70d311d72af3a683c0445baacf0362a1c5426c37218c09d1",
    "deploy/mission-control/agent-runner/work_owner_reverification.py": "957132b1ac1ddfa39d3602cd8137867800026ffce9092f9efb83d486cecf9f2d",
    "deploy/mission-control/agent-runner/work_peer_review.py": "35caedf43d10c81bde72543575eae277002d3e3a201e8926aef6b4813680e8d0",
    "deploy/mission-control/agent-runner/work_retry_continuation.py": "1f428a5697fe8b01ed7b906068ffd67bd811ad99a3c67339237783021853542e",
    "deploy/mission-control/agent-runner/work_retry_policy.py": "8542d515cd522f0183f79f7dc697b40bf9e13308410284171756dbd5d4c86640",
    "deploy/mission-control/agent-runner/work_reviewer_correction.py": "37354a39b14b48a503503480792f7fa76ad806c17bd35d29811e1b51e1ab89c0",
    "deploy/mission-control/agent-runner/work_store.py": "4f16976f616e0bcca58a22b321615a304cd0fd7a5ccbf98f747f45408771486c",
    "deploy/mission-control/agent-runner/work_workflow.py": "f4f42d9b49d35e590079bf91932aecc2158b77cebbc0f4bca747cf2ca94a28e3",
    "deploy/mission-control/backend-suites.py": "4ebeb0107e9b69706fe34dde68b8cc205bc947604973f00c4989cdbbb1271a6f",
    "deploy/mission-control/sql/apply.sql": "7aef74aa55cc026fe056df383b25c7f93ce898830a6f3642d1dd0ecf2d490258",
    "deploy/mission-control/sql/management/verify-agent-retries.sql": "19c517c6b84e3b49f5e35a1bd1e0ea9866e3d2cd9360be35d3e94c8b6b1ab2ad",
    "deploy/mission-control/sql/management/verify-feedback.sql": "56bc19978bfa376f9955b38b15b4df94cc707ce8320fbfbda0b24c4b4f8baf3a",
    "deploy/mission-control/sql/management/verify.sql": "93d45b86823e4099b690131b3adc58c106fc45f448730e9098af08eb337b38c1",
    "deploy/mission-control/sql/migrations/0030_agent_wide_retry_policy.sql": "5a2b07a8be9c5835c5ab90d03d2a85c7936a95e01596d4c550c711b0a2122975",
    "docs/architecture/forge-post-merge-verification.md": "c006f4b60539abf0e4f1f11cfc2a3bc222252fbaf11ac89e35b85d1fac4e6ac1",
    "docs/design/implementation-status.md": "daaf740b8b28d5226e8ec4f8adb06e65539ff65efc7eec2eec52b3fdb0a0cf8d",
    "src/rust/forge-ide-ui/src/protocol/work.rs": "b43ff8969476110078009548158e55870809084cfcc24cbc7c7e85c4ad9c3c63",
    "src/rust/forge-ide-ui/src/protocol/work/delivery.rs": "eddc29ac4734a41df08e7ad16643b93e7da36652e0d1ce80ae8b09b8fdd7b846",
    "src/rust/forge-ide-web/tests/browser/agents-relay.mjs": "36b85d3b3242040831efc148a810c0910d5f06182c9e30f9ffb126f325483638",
    "src/rust/forge-ide-web/tests/browser/journey.mjs": "42aeb1c7750614b287ca2023cbd324db85ef12f2bb6fa91e6c63c1f4eceb7766"
}

AGENT_WIDE_RETRY_GUARD_AST_SHA256 = {
    "work_owner_control.py::NativeOwnerControl.review_decide": "df38231d3ead8bdcbda377ef1caa8169ac2d2ecac79e44c8f49c306f1ff822a1",
    "work_workflow.py::WorkflowStore.retry_peer_verdict": "8cdec9551d14ac203946d8a856e34d702774a21dd51326e4f004318b8aa497a0",
    "work_workflow.py::WorkflowStore.retry_preparation": "548ac8131c562637b7dc80316cc8f49ae4d5b835779ef718bcc3e11fc26a1edb"
}

# Reviewed per-event retry context. Task totals are audit history; immutable
# predecessor source/guard maps remain provenance. These source identities do
# not grant CI success, publication or privileged deployment authority.
EVENT_RETRY_CONTEXT_SHA256 = {
    "deploy/mission-control/agent-runner/test_work_reviewer_correction_events.py": "ba46b09aa9b1917e2872d725210bc504df3ca70cdbacb7f45f897c341547f107",
    "deploy/mission-control/agent-runner/test_work_delivery_automation.py": "6016a566a6cd85e0f38bf4073f4c495cda4cf62a621086d5b1d43f7b29c509de",
    "deploy/mission-control/jenkins/setup-ci.py": "99dbe52ff966ec62202c4f3fa16dfab0623b5a6463c0a990f3141026e7925e4a",
    "deploy/mission-control/agent-runner/test_ci_installer_v3.py": "c887bdbfbdea591a0f80c4cd340a45a9d3b2813878e15cf79771ca30e20bd864",
    "deploy/mission-control/agent-runner/runner.py": "fa8e8499b8f4e7cdebc2e247883d361e1c034ce71aef24c809e4c9d642c99269",
    "deploy/mission-control/agent-runner/setup-runner.sh": "03cfb4d33d579d688988c36800f12d8e55b7993ead6918e2bb771b1e15f4aa20",
    "deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py": "e1000fc0989f05d961b0323c70e604981a364dc297de497c04368802b740c7ec",
    "deploy/mission-control/agent-runner/test_ci_supervision.py": "dd085b67e9b79c040f9946c1401600e4d4266b4618aa9d4288cf510fb3c56584",
    "deploy/mission-control/agent-runner/test_external_flow.py": "32f339f42cdb23f33e3b3d5b5def9a0ecf2546e0a54fbc689ce6ec46d289d8b6",
    "deploy/mission-control/agent-runner/test_muse_read_access.py": "c7573aaa207062bf482cfa15e7404a33d66ab670411010f4ac2c5f343e7a4899",
    "deploy/mission-control/agent-runner/test_portal_release_actor.py": "90ee41284a6dc77acb40f41bf3bbf4ff497366dd97fc4b64902230f1175068a0",
    "deploy/mission-control/agent-runner/test_work_muse_intake.py": "3ee400e72fcacbddf00ff1e669ef2ed48c860623d4903d1f78e930d6d398c617",
    "deploy/mission-control/agent-runner/test_work_owner_fence.py": "4b0561c763a64daa9a8cc5f5d8bd10b906d583914c17b7c384f0746138deb0e7",
    "deploy/mission-control/agent-runner/test_work_peer_review.py": "eb9568cf1663401ecd9b7fbc1f69de23ed09c60e652d969c7ed95b2dcbe7a948",
    "deploy/mission-control/agent-runner/test_work_policy_upgrade.py": "53c393c1a97946f273e712a93a70d2f1ac6006a5ea945cf1430ff2dd53225d6a",
    "deploy/mission-control/agent-runner/work_api.py": "8eb5966f1a5c5cae93db4a0b658e4d3627d9b86d19dbb20d8bf269efca4eb3d0",
    "deploy/mission-control/agent-runner/work_ci_contract.py": "f9b427a05e55bb8c5f52535bfdad13d7b04e9e8d80c6cde41afce67915d25072",
    "deploy/mission-control/agent-runner/work_ci_phases.py": "2fccd2c704725a05e329b64587377acc2ccded53840487b2acc478bb7a85e368",
    "deploy/mission-control/agent-runner/work_context_recovery.py": "6a404f34bce07fde88259e9c67cf59afd6773d41e4388f1dd933af0e26800836",
    "deploy/mission-control/agent-runner/work_external_delivery.py": "e7336851ba870054ac5fc43524d02fd89d2333ba4bc626083706612ef54dbad9",
    "deploy/mission-control/agent-runner/work_external_workflow.py": "9dcea4ac4bb661a4c6cfd854bd07f3881e60bef062ddd23cb2608dad67d3ff60",
    "deploy/mission-control/agent-runner/work_native_jenkins.py": "84ce9cb2c17bfc737e9b3ab6e39deb9165fc5638e3c72469de06086afafde7ee",
    "deploy/mission-control/agent-runner/work_peer_review.py": "2a12dca199e3c0314212bb927d74a06f601f14767c13fc894f96c8a3de40336e",
    "deploy/mission-control/agent-runner/work_post_merge.py": "5956dca2dfa6f63a5c77ec76c056e906c8f0223c744b6100fcd9d56ef94b229b",
    "deploy/mission-control/agent-runner/work_post_merge_provider.py": "8d6fb012d5df68f440803ecd0c1ba653417cf38abc8ff68a3a30a6ee072d0df8",
    "deploy/mission-control/agent-runner/work_publication.py": "15a39a81a8d3f23e6f546f87588950c64d9110da5409cdacf6114eccda4881c6",
    "deploy/mission-control/agent-runner/work_publication_config.py": "9f96505e090b141aaa222dac50d6030e0b19abd1463012c889ff73205fe458bf",
    "deploy/mission-control/agent-runner/work_retry_continuation.py": "d149e3062836b020cd30f0acc0910ddd58ebd744f271c4db0030a9189cb40b17",
    "deploy/mission-control/agent-runner/work_retry_events.py": "4acbf3e6d349fe14f397ed85b4f4eb3c48b944815a17144f8c759866ece846ce",
    "deploy/mission-control/agent-runner/work_review.py": "fcc2a693568907b8d5caddc6d4f4a171c2c63356c021f8189e52153949bb35ea",
    "deploy/mission-control/agent-runner/work_reviewer_correction.py": "d7ab9c6169fe78bf4ce68816c4dfc22acba96fe86f7d0f8e9f6f1168bf38a614",
    "deploy/mission-control/agent-runner/work_store.py": "8940faa1f5f0e3e1ac182c260d197c2c397c42d7e17568da050606c2a969c2fa",
    "deploy/mission-control/agent-runner/work_workflow.py": "ce86e32438d80b17bcf6351cfa33fc4b7bdca205cd9f599bf724fefe0d295d32",
    "deploy/mission-control/jenkins/verify-candidate.py": "c1341a3094280cd6002dc77ae2dc8525b32edcee7283d65e10dbb690fc8215d5",
    "deploy/mission-control/portal_release_actor.py": "87a36d555a25c861d9af541dd4d5186a9c8ca60c9ffccd260941b5289e0bb23c",
    "deploy/mission-control/sql/apply.sql": "b317aa1115d05d277c41dbef3480cc7499c6bc4a388ceb1987263ca12fc4450a",
    "deploy/mission-control/sql/fragments/0031_ci_stage_receipts.sql": "4aed05a59da74265333dab2ff925edfeaa013cd1e8c08465f8a92a750b179230",
    "deploy/mission-control/sql/fragments/0031_delivery_retry_events.sql": "e13ff88d320fc28f8391ce85444ad65233fcae50dc6a85531ef4e6b33d3d63f1",
    "deploy/mission-control/sql/fragments/0031_native_retry_events.sql": "638897abacd36c3841f80ef8021a217dd026018b88fa636af07e18f1aed931a1",
    "deploy/mission-control/sql/fragments/0031_retry_event_fingerprints.sql": "fe04b42d8e956745ad979d7219343f99713eaf2018a0e1fd1fe28c661d0af9e5",
    "deploy/mission-control/sql/management/verify-agent-retries.sql": "b5d29cec97d09f5daae6282d5019851f4e243d2109a212162cd4a307a65f77bf",
    "deploy/mission-control/sql/management/verify-delivery.sql": "358e3f07059cca044adb2a94cc63f57701019e38ea5d96af575baed2f9d244f7",
    "deploy/mission-control/sql/management/verify-feedback.sql": "4244c852fea973317cc780f281e048f249a39985ccedea1df7065a2413dc0d61",
    "deploy/mission-control/sql/management/verify-owner-continuation.sql": "608130e4dfe88ed572c1f98271ce06acbc4d038bb4669975c7eaa63c7c030680",
    "deploy/mission-control/sql/management/verify-post-merge.sql": "177f2da332366b620f6efbac13630b29c304e5d142a243a20081d59c6c7480c8",
    "deploy/mission-control/sql/management/verify-retry-events.sql": "571545fcda5c65fff60ed62cff4e0cf65d334d50a00cd104719d5ab44bf4c6cf",
    "deploy/mission-control/sql/management/verify.sql": "5df98d668b87c0b0878fe34e36738376cb71f4c7cc83c2e2b1ca96b532371b71",
    "deploy/mission-control/sql/migrations/0031_retry_events.sql": "bf6adff966e9f6610ac3e5dd31650b3427b8af5324cd5945f2a22284a9116c41",
    "docs/architecture/forge-post-merge-verification.md": "db9887fee35cd3ebcbf7a2f1cfbc2452c8e163374e3840a8160deea04c2967c5",
    "docs/design/implementation-status.md": "e0b487762cffdf7d32ebe9cc68260d6786d935a2d34b1544d61ff9a60b050b9c",
    "src/rust/forge-ide-ui/src/model/agents.rs": "df4f384d4fccd8afedb01dcff5317e0b675de7d33f9dd943ae6639df33d0777e",
    "src/rust/forge-ide-ui/src/model/agents/delivery.rs": "a32081673201aedffd18bf0937243b2dd71cb41edd3ffbe2bd2796a12a3d3efd",
    "src/rust/forge-ide-ui/src/model/agents/tests.rs": "8a84dbe37b6489edf580674a0d7b48d73f3a8d357414c575071387ee15c55284",
    "src/rust/forge-ide-ui/src/model/agents/tests/delivery.rs": "eaf72e886fbd80e3c78bfe0e4ee8bd1960dae9e20e4f01f58e3f9c6995bd60b4",
    "src/rust/forge-ide-ui/src/model/agents/workflow.rs": "bc24fadbd6254b4885d665de9cbdbb23489ec1fa9283baf7601613f05a5e503d",
    "src/rust/forge-ide-ui/src/protocol/work.rs": "c1fd35bd4ca84cd696a723c83b0ee2e53dc3339079a81a86cc2ddbe0d4855253",
    "src/rust/forge-ide-ui/src/protocol/work/delivery.rs": "afd99590187b2c6e2a2bd4d1315333e4f622e376ac0a898cdb7ff0d2907f7b6f",
    "src/rust/forge-ide-ui/tests/wasm_abi.mjs": "65e17657d542e26f89b87ab67a484e61950f4a273330807005ae577c47b0ff16",
    "src/rust/forge-ide-ui/tests/work_review.rs": "2d636f315778c4b05b3391f7f73d13769ea553a54a86dfb128e3792fda8eda67",
    "src/rust/forge-ide-web-host/src/assets.rs": "0d4a1e8e990229276345223a5ddb6a5ef8100d865dce004da1a8581561d4af3d",
    "src/rust/forge-ide-web-host/src/chat.rs": "f1294d442317b6163c1e13388580adbea0aee427d99d1440a6d8c7eccaced15f",
    "src/rust/forge-ide-web-host/tests/consumer.rs": "b99d76c127795092f790854c410a8bc11d9af8499b4dd8588b0c82f4551baf56",
    "src/rust/forge-ide-web-host/tests/consumer/work.rs": "12d67cf7768e7cc513fce2a11c4dc51aad1d53da847010c5aed072a37f6b05f1",
    "src/rust/forge-ide-web/src/generate/application.rs": "610dbfa80bb17352250bb28927765ec1dbc7a3d02dc37da0c8ecff3fb4ab9b3a",
    "src/rust/forge-ide-web/src/generate/javascript.rs": "b08025b0c9d5e34551c5e5eef55fc2f16704328f4b3d866424b14da1ef6c3804",
    "src/rust/forge-ide-web/src/generate/mod.rs": "e1bc357350d0b302c972baa47ded9d0fd32a743f7f7a0c20507aaec7972363d8",
    "src/rust/forge-ide-web/src/wasm.rs": "8d2667307a56ecfdb38ff5c4ff7470983feda2364997c47cc6d5d8191c086aaa",
    "src/rust/forge-ide-web/tests/browser/agents-relay.mjs": "b22e7128f3bb4126c2f6f91160b35594d36392a844f837030c95f52bb87778e0",
    "src/rust/forge-ide-web/tests/browser/journey.mjs": "b8aeeac03349d36312a0646b5de369c229e5c2875b55dfacd8a35ec76840dda7",
    "src/rust/forge-ide-web/tests/generation.rs": "e099773f1bf06525cc92dada0bc8d9ed5597b533a85919f6757ad9c9bffbbc0f",
    "src/rust/forge-ide-web/tests/playtest-wire.mjs": "048817854abba0d5dff68dc42df4e3abae8b5768b13ad3b3bac945fff71df1f5",
    "deploy/mission-control/agent-runner/work_events.py": "260935af16682f9eefdec86e0e206095ba17cd5857187b04560b41e7366cde2b",
    "deploy/mission-control/agent-runner/work_jenkins_lifecycle.py": "5bd39e75de72dff2e66a64d553b4191ff8461cad6291ed94382a1fd9a8cd0494",
    "deploy/mission-control/agent-runner/test_work_jenkins_lifecycle.py": "6b3d602b17c1de6497d9a2030d520aa96e8874f27e37885fcdc83315a59c707a",
    "deploy/mission-control/agent-runner/test_work_event_attribution.py": "3d669cbb7ea162e2214422038247bea2faf32cec8fb9ec92b4b8aab7111edd14",
    "deploy/mission-control/agent-runner/test_work_delivery_contract.py": "73d59d60da1688fc8580b2f33175beef4366463b07c154d425d8bf96e0245775",
    "deploy/mission-control/agent-runner/fixtures/peer-target-351b8c41.json": "11d3542aafe789669496eb6bb667955f51e19f747dbd1c928f19f9361b87dfda",
    "deploy/mission-control/agent-runner/fixtures/peer-verdict-719aa2b2.json": "6091f0640084b193335e67accc030e477e03be53aa90baa94982705ba1e2bfef",
    "deploy/mission-control/sql/management/verify-autonomy.sql": "3780ff5f6185c023e719add87b5fcc6619e32f40c9ec00f2ddd636d482f752e3",
    "deploy/mission-control/sql/management/verify-external-agent.sql": "6eefa483b2d5a0ce29c84940e465d139dc6d830ad88aee4bedaafeaed3badec7",
    "deploy/mission-control/sql/management/verify-external-workflows.sql": "adeca1e1c7ddec60a29496c5c7b45b00c880dc6f90318891a841f71e855e3e6d",
    "deploy/mission-control/sql/management/verify-jenkins-events.sql": "693b551e708138663c75fc274b2b7bc55e0cd70f10358081878b0be49074a0a4",
    "deploy/mission-control/sql/management/verify-task-events.sql": "aeed47e3b810c68223c1d46108e5398d8dfa7d768f58cf145f386a780fb594a0",
    "deploy/mission-control/sql/management/verify-agy-unattended.sql": "c098e668cdcaf965308a9a3b1b79de25211e1a80042db91f647da68c3d5c03ca",
}

EVENT_RETRY_GUARD_AST_SHA256 = {
    "work_workflow.py::WorkflowStore.retry_peer_verdict": "5972f045d86247d74e2d7008307fbe51b2ed5980123b666cc19f9ccae36d9771"
}

# Exact additive M32 source; predecessor maps remain historical provenance.
# Source identities never substitute for real CI or deployment receipts.
LINEAGE_RETRY_CONTEXT_SHA256 = {
    'AGENT_WORK.md': '01e68b6a41f72847fb6e9a70fad7d2c3f2aabdeaa603d16fa12180edd2a28515',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py': '2b1c724ce4520d577004ab2cfb451b2d89581052aff001a4bccc4a7b11852143',
    'deploy/mission-control/agent-runner/test_ci_event_phases.py': 'ce2a2595160ef3bdefef48cc3691013a11146de5977c0c81f09677a717527c04',
    'deploy/mission-control/agent-runner/test_delivery_lineage_events.py': 'f2f6f2639976d6ca6d2e43779c3be44f76fee7bbef46c8fd1fd2789301036a42',
    'deploy/mission-control/agent-runner/test_delivery_lineage_schema.py': '21151b2f6f948243a04b43c3639df0959a718b5f52fdb2d437daa961a297f86f',
    'deploy/mission-control/agent-runner/test_muse_read_access.py': 'f86f55d35ef8ad4d27b57dd69a89fd6d53dc839485c5bdc941fcaed4bf789c14',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py': 'edf3c9a5d911251eb31a51078b58ef04ffb8cbfce63e18d7de82282fbe5ace8a',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py': '1f7ab99858ef7ead05d531b9c85fa2a5377834850f0fe5a1ab0aa669c42509f3',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py': 'de99aabf23334e84baf38c7258c1cafe308a51535fb51596aea8159293317121',
    'deploy/mission-control/agent-runner/work_ci_contract.py': '2ca9f31ecb7b9c0f2ad2238e7e577fbe693ed28c93e65b5809fd32cec82e2b6e',
    'deploy/mission-control/jenkins/verify-candidate.py': 'e438b506c12d108de13ef8c272a5b6c58ae613708bb82a6b622d1322fd29d60a',
    'deploy/mission-control/sql/apply.sql': '074f4f938b8bd108e165285a2380f417dfa34e7ab59daf4a1e0038d44ae45516',
    'deploy/mission-control/sql/fragments/0032_observer_retry_lineage.sql': '2313c56322a9f339ca46163d443fc036be18d92a7d807831d9bb4b59a788d0d5',
    'deploy/mission-control/sql/management/verify-delivery-lineage.sql': '61facd8165eef6cf478bd2d12d60bb937c48c5c548dad8a21acf3750bf11b67b',
    'deploy/mission-control/sql/management/verify-delivery.sql': '6f4241976a6b5f802e5841ed96f0989c4a8db8819a84def148e46b1742efc56f',
    'deploy/mission-control/sql/management/verify-retry-events.sql': '7ed764895e67c0b5ed2a5b8872cf451ce5f85306abb87316c36ffbcbb75ed13f',
    'deploy/mission-control/sql/management/verify.sql': '215ea6880b3b356f1f2a3756d53ce01681f94dd7315a6ed1f9e4b278d5ce28aa',
    'deploy/mission-control/sql/migrations/0032_observer_retry_lineage.sql': '88705c12ceb94b3264d5af084e0cf08bb095a10ec74460099c54da3dfe64bfbf',
    'docs/architecture/ci-stage-evidence.md': '976d0728e0522e967363559adeadcf033738e86f156e668e54f228c293944966',
}

# Exact additive M33 source; every predecessor pin remains historical authority.
SUPERSEDING_REVIEW_CONTEXT_SHA256 = {'AGENT_WORK.md': '7bc6d4ee90235797dcbb0ab75a0759943cc3e230a9b9f7febd6490d809d7fdb1',
 'deploy/mission-control/agent-runner/setup-runner.sh': '4982edf6698c318f72d3cc06285448326ea090ba84dfffd1a74ef257691265ef',
 'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py': '2a17322ef5c3776e374dbb5e631fc89d1c7d83b484ea195a7286d37b3675b1b1',
 'deploy/mission-control/agent-runner/test_ci_event_phases.py': 'd39fd595d4ba86d13aaa5b99621e8b9e87bbd5aa9504bf290f47436f5c4645e0',
 'deploy/mission-control/agent-runner/test_delivery_lineage_schema.py': '9e3d529f0275bbd8245ec20974f648b376931809bbd58fc9775f9b3411cac772',
 'deploy/mission-control/agent-runner/test_muse_read_access.py': 'b05ffd90e71452efc62a0ef01d7fcce7581b2d5e792a442892285305ab016aba',
 'deploy/mission-control/agent-runner/test_superseding_review_schema.py': '8745bb2d276dea408b57eac3dc1b591984b5b909b74ce06f8664cdc93b24c6c7',
 'deploy/mission-control/agent-runner/test_work_muse_intake.py': 'afef02c708f844467c734d8b9e02aac0b67f36a2d6e59a61bbade727086d7463',
 'deploy/mission-control/agent-runner/test_work_owner_fence.py': 'e8312f05f22e85d1bf11e4b44d2ac21d5b407fb8d17a79c545ed7c3c0fb1bfeb',
 'deploy/mission-control/agent-runner/test_work_policy_upgrade.py': '6224514db83428e9127eeccda3bc2c73aa08416a817e618d26ca2cf50d784a45',
 'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py': '2b280277939b97b6a4c738eeb22fc5d00cbd7b9038d2cd4ba0adbf7a4bfe0590',
 'deploy/mission-control/agent-runner/test_work_review_transcription.py': '5013c229831d18f392b8936ccc2708cd1faed277c3bdb570a658ea7464a4b680',
 'deploy/mission-control/agent-runner/work_ci_contract.py': '05cc57f0a30694d75f57767a1988f4bb265c76d890f670930e6edae3a9677299',
 'deploy/mission-control/agent-runner/work_superseding_review.py': 'e792a42c7245ef76025aa7810cf7570a772d755d55c78d12adba9f361f5a252d',
 'deploy/mission-control/agent-runner/work_workflow.py': '12de226b95b1e6d4b1e24d86011be94c89dfe80225d008a1d07f31f7e472e400',
 'deploy/mission-control/jenkins/verify-candidate.py': '2100f323f4c264ddf9f55621c103853ebce4e75f2eeab7cc0f283f0d3b41a472',
 'deploy/mission-control/sql/apply.sql': 'c7a34eecfae5174b5b8b927fbd5f28639abd8aaa2d3ad724158f94a1728a1de6',
 'deploy/mission-control/sql/fragments/0033_superseding_reviews.sql': '3bc5b801a55f1ab636f898f3ca9c6f0b541e0e5e62efc4f56b743c005f9b0a57',
 'deploy/mission-control/sql/management/verify-autonomy.sql': '677d28afcfdbb2388c204f911013a68e3016b97ee5793d80c54a36b278cffb51',
 'deploy/mission-control/sql/management/verify-superseding-reviews.sql': 'f536268c5aafff1b357b08bd80509879cdd87824604df9ac2c72c89871aefd22',
 'deploy/mission-control/sql/management/verify.sql': '7043ae6759a8f93a6d090fe8544948a3508323df33548c6538c98073678ad173',
 'deploy/mission-control/sql/migrations/0033_superseding_reviews.sql': 'c8cc28b5e0aeb618ffe43725452935024ce66973420120181f01b3d887edf30e',
 'docs/architecture/proposals/forge-autonomous-agent-workflow.md': '7f2383cd89be8a5549a3ddd391d21ffc5d4970624a125450dbd26484e4d9cef7'}

# Exact peer-feedback correction; all applied SQL and predecessor pins remain immutable.
PEER_FEEDBACK_CONTEXT_SHA256 = {'AGENT_WORK.md': '5b2736ec7bbb7502c3a8ecaa9a8b99637dd8dfc0f8151758e90186a5404dc877',
 'deploy/mission-control/agent-runner/setup-runner.sh': 'f7035d6ed8070811cbdc430af94bf08f16adb74019de4eca44db16f8c2f6e73d',
 'deploy/mission-control/agent-runner/test_ci_event_phases.py': 'fb8d24dd8427d660281a597f535de374519e595d6328198f5458613c2fac7b72',
 'deploy/mission-control/agent-runner/test_work_linked_child_feedback.py': '54bf5a07a1864ef1d17c2eb4e791a677f1241d34a388ca02f6cf8996f6e591ec',
 'deploy/mission-control/agent-runner/test_work_peer_context.py': '0d01c0eb9b5a7f40761add138db5098b587713b2330b1a53552e62b931be021e',
 'deploy/mission-control/agent-runner/test_work_peer_feedback_recovery.py': '498a27d4c63143d6cfdd17130ccadc1c24ab6ba30607b857c44ccf11f4855b4e',
 'deploy/mission-control/agent-runner/work_ci_contract.py': '97bb410ae046980f03ac20d4659ca5f717afedd2b56e205e2825724cec3e13d6',
 'deploy/mission-control/agent-runner/work_context_recovery.py': 'd0c8c83b749afd6fba5fabad5fc71559944b42b8ade14c5e441ecf55fab69f9c',
 'deploy/mission-control/agent-runner/work_peer_context.py': '298a96174c015a5328ef927d3c662fca6fa609b9b6d5ba5d9d01e9934c412f58',
 'deploy/mission-control/agent-runner/work_peer_feedback_recovery.py': '5cf99d4949f63f151ab3d99a496a7cd3899576cd5d5628e5fa612645c382c3aa',
 'deploy/mission-control/agent-runner/work_workflow.py': '51b0b1a68e947681ae007624da53aac8371b6986d733f9b005e98e600bfa669d',
 'deploy/mission-control/jenkins/verify-candidate.py': 'ee759e4406f428134c111c617c8fbb7fb2c0eabe916611345b4c6a3bb75abcb8',
 'docs/architecture/proposals/forge-autonomous-agent-workflow.md': 'dd604a82143980feaa9a4591987516d823685aa7413af2c56e8ab5877aa22210'}

# Exact reviewer protocol and transport fix; predecessor authority and applied SQL stay immutable.
REVIEW_PROTOCOL_CONTEXT_SHA256 = {'AGENT_WORK.md': '74187a9baa76afbd6aae600a2b89ccadd2f95b55d6c3f8e8f71b63d512c4b0c0',
 'deploy/mission-control/agent-runner/README.md': '28969daa3708e10984efb830fc442a6d6784a0c18e5969a4ffff6fed2bb89845',
 'deploy/mission-control/agent-runner/setup-runner.sh': '97c695cc59ad59df8a5fb37e51eb14a06e3e57aa53f0118c2ce1321f3e78a588',
 'deploy/mission-control/agent-runner/test_ci_event_phases.py': '105deceff4ad241e0dcf2527c0db2dc7a74363e6d3af6e3271b342a254413c6e',
 'deploy/mission-control/agent-runner/test_external_transport_recovery.py': 'b0c4a451b07731dfb47c78ea0b871f78e6a652ffa3505bb2f42e5a45c2619aba',
 'deploy/mission-control/agent-runner/test_work_cli_transport.py': 'f0e44d7c7121ca08bf8935ed65205c9f9cf8b42c76f1359a23ed74ed458fde40',
 'deploy/mission-control/agent-runner/test_work_review_revision_attestations.py': 'ec59bc12275322938a4fa6caa7c4443870d6e27e35c7536cc0dbd7a42a0feea5',
 'deploy/mission-control/agent-runner/work_ci_contract.py': 'cdbb4aaa14af50e3e00d2bca43414f86cc9dece38b53d3be2bd0b314635a867a',
 'deploy/mission-control/agent-runner/work_cli_transport.py': 'ef7040213c882906c8c39ff45e6a8fca8b8d2b212586e7caa3828b90ef1451f4',
 'deploy/mission-control/agent-runner/work_executor.py': '0fd960cd7f9eb504f365a589f2229ac0f4e67582f0a74ae234f43986f22367a0',
 'deploy/mission-control/agent-runner/work_external_recovery.py': '2678b27800d3342e7cb9c1962513fce50876f097707e4230a0605544b1e565a6',
 'deploy/mission-control/agent-runner/work_external_workflow.py': 'aedae273ab85dfa8389642cbb74e76702490063c9e1c536b8ea20f1f844c4906',
 'deploy/mission-control/agent-runner/work_superseding_review.py': 'ca6b13e6f2e84ebc9c25b02687d2c9910f9c8185953b11c074cb3bcd26a6e382',
 'deploy/mission-control/agent-runner/work_worker.py': '12ba5bbf801e9f07e9154f14a4bc3e440a3432473cee023288b69c0b2ba3c12d',
 'deploy/mission-control/jenkins/verify-candidate.py': 'dc5ccf97c683fdd6c13cf39b4c67fbe062673638c42399b8a52260f030e81bf1',
 'docs/architecture/external-agent-workflow.md': 'c4811902a3ebabde6271f67e6ea3f0a7a443bb35d17e378ea5ccddfd721e21ef'}

# Current additive ABI15 event-stream stage-diagram UI source; historical maps
# above remain literal provenance. This narrow overlay binds only the UI model,
# declarative component/emitters, atomic ABI15 metadata, docs, and UI/host tests.
# It grants no live activation or CI success and changes no SQL, native
# publication guard, or backend source pin.
UI_STAGE_DIAGRAM_CONTEXT_SHA256 = {
    'docs/design/implementation-status.md': 'a57eb8d4df855dd32c0a27c59b6f0baee0698be2901414cb2644979a2b1a9330',
    'src/rust/forge-ide-ui/README.md': 'e952faf199fa1ac5ed4deeab6fbb2b566dc60e94f1c09739fab56548368252d4',
    'src/rust/forge-ide-ui/src/model/agents.rs': 'ab8c02cd2af6d7512c057cf6cac56851393eee6ee47b227874038c99e4501316',
    'src/rust/forge-ide-ui/src/model/agents/history.rs': 'd4fe5e7cd2a9464c3e79ee3145cfcd2064a1aeae6534cd44818229b9287e775c',
    'src/rust/forge-ide-ui/src/model/agents/stage_diagram.rs': '9b8363ef2dc50f22740e0e5ad4b0b381a76761b8ef078205c9df268c42d3db4e',
    'src/rust/forge-ide-ui/src/model/agents/tests.rs': '5a93853e0e386f79fe72487cffbbeef57ef76f9b07802563a10bdfb325bdc1de',
    'src/rust/forge-ide-ui/src/model/agents/tests/stage_diagram.rs': '1ef877f85ad81b65befd616636ea996794077aea22a8cdf1b9c0183544986ca8',
    'src/rust/forge-ide-ui/src/model/projection.rs': '9ea4d08b8f14a1d7d7b6ff36bcf5379209a2219b7496f547aa4210cb52801d33',
    'src/rust/forge-ide-ui/src/view.rs': '85156fe7301b359e98f6537782bb7cd61684ef4779bb12aac2f033422c1a05ab',
    'src/rust/forge-ide-ui/tests/worker_glue.mjs': '44e733ef50d84c2457ebf98db60d3591309ed03275925ee89ee000f546968831',
    'src/rust/forge-ide-web-host/README.md': 'cc9dfbe13b50803c1d2910427846972d7d42c9b0fedb623aeb141ba5f1c102f6',
    'src/rust/forge-ide-web-host/deploy/README.md': '0d0dda823d829f0e27fe0795e16a4d4691888766aa32cf07991b6fee06cadf39',
    'src/rust/forge-ide-web/README.md': '2feb9e100fa0368ebe3963b3865ede6bc5be8af5f672acbfa57c4eba919323a4',
    'src/rust/forge-ide-web/WORKBENCH.md': '1adca87335dee88b73400861d7bddbf9864573803b7cd9110b83b54119db62fd',
    'src/rust/forge-ide-web/src/generate/application.rs': 'f9b5556d89842aa8da46d64d8a11273dd8c4176dbb5957c7269cbbcd551c7a28',
    'src/rust/forge-ide-web/src/generate/component.rs': 'f7af659a222113f7e7c5ef19688ab15fba59c71705f1095dd0555eae5a52acbc',
    'src/rust/forge-ide-web/src/generate/html.rs': 'ad0cd101d75d0ff8ff72b1c0af0255536089d29bfa23dc65751d0ac946254e99',
    'src/rust/forge-ide-web/src/generate/javascript.rs': '737465499080e9bd5e3b7132200eb2f5c6d7da9590ceedc244a93f3be35b9b9a',
    'src/rust/forge-ide-web/src/wasm.rs': 'fa73e7e3c198aca0e62bafc46dd02e9a9780599961a2d3460010dac45304815d',
    'src/rust/forge-ide-web/tests/browser/journey.mjs': '2f97ec908a7bc13de5162b1ae0ce4425bf35057bc76ca75657ddc6d30a8ab5d7',
    'src/rust/forge-ide-web/tests/generation.rs': 'd76abe1dd440806fe0de94d88286b8da9c5af467f8ca2508c9a6305cde4fbab0',
    'src/rust/forge-ide-web/tests/input-presentation.mjs': '8533c2c2f2ed0d1d591e1a115631819ea7548ae7f838c5dad483b8accbbff31a',
    'src/rust/forge-ide-web/ui/styles.yaml': '64e800eab1eea5422188f69ffd687a75ad867939eb5b77f2c67903f8788f71d7',
}



# Current additive Jenkins pipeline stages diagram source; historical maps
# above remain provenance. Binds the stage_started migration, the lifecycle
# producer and drain, the stage emitter, the server and UI projections, the
# ABI 14->15 move, and their tests and browser journey. It grants no live
# activation and changes no legacy owner/publication guard.
PIPELINE_DIAGRAM_CONTEXT_SHA256 = {
    'docs/design/implementation-status.md': 'a57eb8d4df855dd32c0a27c59b6f0baee0698be2901414cb2644979a2b1a9330',
    'deploy/mission-control/agent-runner/jenkins_api.py': '0ceaa309ea41de4e79583fc6a2e6db9c441a683489f8179c670a666320846ad4',
    'deploy/mission-control/agent-runner/jenkins_events.py': '295d052ca4b693db60c1e55afa94939b62e48b458b1c47d78a9768889f828012',
    'deploy/mission-control/agent-runner/runner.py': '20a84ac0699d6be2643b12ce1b7d0eb9d057fd682e7f9522e4881420fe95cd65',
    'deploy/mission-control/agent-runner/setup-runner.sh': '97c695cc59ad59df8a5fb37e51eb14a06e3e57aa53f0118c2ce1321f3e78a588',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py': 'eed7dd4b74a6463061c178139623b24f611230c58cf935b1cffe68079afe0bfa',
    'deploy/mission-control/agent-runner/test_ci_supervision.py': 'f1ca0bffcc886768a9c91f209f77c82e0af31ed440172d0d5af87b58d89966ee',
    'deploy/mission-control/agent-runner/test_jenkins_api.py': 'c66ef69224bc5792321f8343bd27b98c9439d1347793eeb8dd8021c21faac494',
    'deploy/mission-control/agent-runner/test_jenkins_codec.py': 'e3c94bc68e875f27875d4b078517472edda6ee9609e4a94bf15f036c76d49e4a',
    'deploy/mission-control/agent-runner/test_jenkins_pipeline_lifecycle.py': 'bad9c600c09f61c0d1611ea6cba3aa3d3bce9c8d57b829968984c440c092b082',
    'deploy/mission-control/agent-runner/test_jenkins_stage_emitter.py': 'c0c2fb868da60f4e8e5721c26b9f8046482d712699656376b5fbf6b76b8e8e10',
    'deploy/mission-control/agent-runner/test_jenkins_store.py': '82c546dd0e562642c8eb14687792c8e24683a98647f1a13454d7329c8bd7ddcd',
    'deploy/mission-control/agent-runner/test_muse_read_access.py': '96ce11ee2a09a069ee20baa7b69f12cdfa89a1880ed63f44133dccfb0d31ccf5',
    'deploy/mission-control/agent-runner/test_pipeline_projection.py': 'f2de8675e3a259a6d7eee53855d1216144eac0382c863ae8d8795c22f4005ab4',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py': '15b5af87462eb25afeb650c9593f0cff925cc18b7d2f1cf0386c9def4249d64f',
    'deploy/mission-control/agent-runner/test_work_native_jenkins.py': '31394ee6b3e410ca980719018b4d1247205dcf3fd8d2040a26235291c3240fe7',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py': 'caf6cac33c4536b6686907b9c6c41d7903f15fef0b8dd0a1f0cb5f00c9453691',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py': 'b68bb80f0e21ddf7a21433f621c935889c059e7b8d8ab73bf37797b7777e716e',
    'deploy/mission-control/agent-runner/work_external_delivery.py': 'b04e05184b716b3b705296803afd4dbc23c6d21b77ecc5ae4aa1cea84b1fbe8e',
    'deploy/mission-control/agent-runner/work_jenkins_lifecycle.py': '4947f039f5da27cbbe6c02b0d55d138a089152befcfa2634bf7c047b882535b5',
    'deploy/mission-control/agent-runner/work_native_jenkins.py': 'd759c066ec2e521e75f47baaecdbe6d5cd49795fbffe4e4f1dbf430b8a429759',
    'deploy/mission-control/agent-runner/work_pipeline_projection.py': '0f1face783c26f9c5bf93d070d19f7569bb429e11cec1579de19f7774c4a5bce',
    'deploy/mission-control/agent-runner/work_publication.py': '83a1f0e8b6314bfc9ae02ee503bfe1d11e83bae071eb4c664a3c5fe51aea5605',
    'deploy/mission-control/agent-runner/work_publication_config.py': '4f856692b6bbbb02ef79bffda774b7b1975cd18091dd6a1715fac950e3696a34',
    'deploy/mission-control/agent-runner/work_store.py': 'a48b25beb0f95c0e36b3f614c6cfb98d103a296cf077aadfb591cf2d78af47f8',
    'deploy/mission-control/jenkins/setup-ci.py': 'c93d822cdb7b565b0d87f5ef5b22c4cc873dd53fb9b5f3759f33d97258d0b8c9',
    'deploy/mission-control/jenkins/verify-candidate.py': 'dc5ccf97c683fdd6c13cf39b4c67fbe062673638c42399b8a52260f030e81bf1',
    'deploy/mission-control/sql/apply.sql': 'c7a34eecfae5174b5b8b927fbd5f28639abd8aaa2d3ad724158f94a1728a1de6',
    'deploy/mission-control/sql/management/verify-jenkins-events.sql': '1821ff76f1a1295b48b1d6fe4d7c252c9f92c0c869f0484d12c8d99429de62ac',
    'deploy/mission-control/sql/migrations/0034_jenkins_stage_started.sql': '5e921a74ebe2d09f1e0db2d4ac793ba826cff6a7343509aa390fde8374cc512e',
    'src/rust/forge-ide-ui/src/model/agents.rs': 'ab8c02cd2af6d7512c057cf6cac56851393eee6ee47b227874038c99e4501316',
    'src/rust/forge-ide-ui/src/model/agents/pipeline_diagram.rs': '0a6ead2b44eafd336231b9d46c4dcfc066455ff26b535912a4455fe91ba6a05a',
    'src/rust/forge-ide-ui/src/model/agents/tests.rs': '5a93853e0e386f79fe72487cffbbeef57ef76f9b07802563a10bdfb325bdc1de',
    'src/rust/forge-ide-ui/src/model/projection.rs': '9ea4d08b8f14a1d7d7b6ff36bcf5379209a2219b7496f547aa4210cb52801d33',
    'src/rust/forge-ide-ui/src/protocol/work.rs': '0cf774f23e223d94e115f522d0a4db4cf93ba27a7d5b4cfc56504ebd9d09b9bb',
    'src/rust/forge-ide-ui/src/view.rs': '85156fe7301b359e98f6537782bb7cd61684ef4779bb12aac2f033422c1a05ab',
    'src/rust/forge-ide-ui/tests/wasm_abi.mjs': '65e17657d542e26f89b87ab67a484e61950f4a273330807005ae577c47b0ff16',
    'src/rust/forge-ide-web-host/src/assets.rs': '0d4a1e8e990229276345223a5ddb6a5ef8100d865dce004da1a8581561d4af3d',
    'src/rust/forge-ide-web-host/tests/consumer.rs': 'b99d76c127795092f790854c410a8bc11d9af8499b4dd8588b0c82f4551baf56',
    'src/rust/forge-ide-web/README.md': '2feb9e100fa0368ebe3963b3865ede6bc5be8af5f672acbfa57c4eba919323a4',
    'src/rust/forge-ide-web/WORKBENCH.md': '1adca87335dee88b73400861d7bddbf9864573803b7cd9110b83b54119db62fd',
    'src/rust/forge-ide-web/src/generate/application.rs': 'f9b5556d89842aa8da46d64d8a11273dd8c4176dbb5957c7269cbbcd551c7a28',
    'src/rust/forge-ide-web/src/generate/component.rs': 'f7af659a222113f7e7c5ef19688ab15fba59c71705f1095dd0555eae5a52acbc',
    'src/rust/forge-ide-web/src/generate/html.rs': 'ad0cd101d75d0ff8ff72b1c0af0255536089d29bfa23dc65751d0ac946254e99',
    'src/rust/forge-ide-web/src/generate/javascript.rs': '737465499080e9bd5e3b7132200eb2f5c6d7da9590ceedc244a93f3be35b9b9a',
    'src/rust/forge-ide-web/src/generate/mod.rs': 'e1bc357350d0b302c972baa47ded9d0fd32a743f7f7a0c20507aaec7972363d8',
    'src/rust/forge-ide-web/src/wasm.rs': 'fa73e7e3c198aca0e62bafc46dd02e9a9780599961a2d3460010dac45304815d',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs': '42f6ad7ba5706ea6eb333305443b9ea545b22c05d95e497dddd341880b75d951',
    'src/rust/forge-ide-web/tests/browser/journey.mjs': '2f97ec908a7bc13de5162b1ae0ce4425bf35057bc76ca75657ddc6d30a8ab5d7',
    'src/rust/forge-ide-web/tests/generation.rs': 'd76abe1dd440806fe0de94d88286b8da9c5af467f8ca2508c9a6305cde4fbab0',
    'src/rust/forge-ide-web/tests/playtest-wire.mjs': '048817854abba0d5dff68dc42df4e3abae8b5768b13ad3b3bac945fff71df1f5',
    'src/rust/forge-ide-web/ui/styles.yaml': '64e800eab1eea5422188f69ffd687a75ad867939eb5b77f2c67903f8788f71d7',
}


# Reviewed branch-only compatibility correction. Historical maps remain literal
# provenance; only these explicit current bytes may supersede their source pins.
# M35 has not shipped on main. No applied migration or guard is qualified here.
MUSE_MERGE_COMPATIBILITY_CONTEXT_SHA256 = {
    'AGENT_WORK.md': '69ff065737bc8933a1d0b4fc46e1d36d627251086291ce15950c89bb2cd6f66d',
    'deploy/mission-control/agent-runner/test_superseding_review_schema.py': '40a5a4ce294461a8da7b16f330fb96d39c57953387d482f912a63a0b6839bc5a',
    'deploy/mission-control/agent-runner/test_work_peer_diagnostics_schema.py': '6669fd97ab41d7408727c0b4c71e99d1cb8982c19894b8be9ef73e403b5a2024',
    'deploy/mission-control/agent-runner/test_work_peer_replay.py': '6f4cc5f4d160fa67301c827fa49d36aad0efe49f4d8ea1816820931b6fc5dbeb',
    'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py': '639b83bb28b870c7931f28041d8690a189b87be02bd68284cfcf2111cff58dd1',
    'deploy/mission-control/agent-runner/work_workflow.py': '98aae238dec240803a3d2dff223be145b340243e55423b7c004befa74091397b',
    'deploy/mission-control/sql/management/verify-autonomy.sql': 'd4ea5e110f74934f3a8b7626b444436c2cf17cb7ae538c49aa65829beffc1c11',
    'deploy/mission-control/sql/management/verify-superseding-reviews.sql': '27ba8c6b9594fb0e607e60a551ea0aa4d15cf11809b077e1027c8e936da0b77f',
    'deploy/mission-control/sql/migrations/0035_event_stream_peer_review_diagnostics.sql': 'bb30872c207b9dd7b510430dde93761567c03c46b72424b305025923c6c07957',
}


# Exact additive M38 external plan approve-override source; all applied SQL
# migrations (0001..0037) and predecessor maps remain immutable provenance.
# Source identities never substitute for real CI or deployment receipts.
PLAN_APPROVE_OVERRIDE_CONTEXT_SHA256 = {
    'deploy/mission-control/agent-runner/setup-runner.sh': '7bc8374cfa1a57ac8b8c3f8e4f18b6c08e6670d09d3089244c1b30a01f96acca',
    'deploy/mission-control/agent-runner/test_agy_unattended_db_contract.py': 'cceb3be8f50a7d5ac3d4808f80e97e3a9c5cf71491e67e373f6a9de302d0a8ed',
    'deploy/mission-control/agent-runner/test_ci_event_phases.py': '57b329da7b011c3a9d86e1a64a824c5c2c0b84bbb3ee604683db84708ca7125b',
    'deploy/mission-control/agent-runner/test_delivery_lineage_schema.py': '3febc65ecd592336922c2c29f3aa6a30ff689271f18015ca80d8ef08e3f8e1a9',
    'deploy/mission-control/agent-runner/test_external_plan_override.py': '7d034724a521ce4f344a8bb581063abc0c8fd5da97eefdf0eaf95adaf8246164',
    'deploy/mission-control/agent-runner/test_external_plan_override_cli.py': 'be8c5c74ab3fa6a205e29bbbf4d7fa0337ded4349374d960fc613d6d9872c3a4',
    'deploy/mission-control/agent-runner/test_muse_read_access.py': 'f4ca5825e8c8b6f94012e24a26ed4da38946e44f576de446146d0a663ddaef9a',
    'deploy/mission-control/agent-runner/test_superseding_review_schema.py': 'ae97a02578470a53c7de26df008efb74f120744a3427d308a1e98ed5b78539c6',
    'deploy/mission-control/agent-runner/test_work_muse_intake.py': 'c4e32af1469aada4c0c4b96894237de7a802a372a6e94cd17c016f94527324f6',
    'deploy/mission-control/agent-runner/test_work_owner_fence.py': '2e6ced3e81b3a501fc46ad3841b1b7f4aafba18ff8b926486ff44077a2b465a3',
    'deploy/mission-control/agent-runner/test_work_policy_upgrade.py': 'd5c1149fd40f75e9128959021b4c782d042858b0114ac8b5c4cf8f5e4168ee9f',
    'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py': '00ba5200fb09b8e97025a71c9ff1660176d3671fcfd8b6747cf0ad83f3f74b92',
    'deploy/mission-control/agent-runner/work_api.py': '8a1be138528da0294ed4ea3ced0a6ecb7045d73467c071ba44a5e64d9217fee2',
    'deploy/mission-control/agent-runner/work_ci_contract.py': '3a5a61a5998acd68aa8628ccc5cb3b6aa7e03795f1d8436f0af6e1ceac2fc4df',
    'deploy/mission-control/agent-runner/work_external.py': '819b465e7518028051f3dfd972cc3021f02a7b2485ab69b8b0d9081a3601b29b',
    'deploy/mission-control/agent-runner/work_external_plan_override.py': 'd7ac2a2aa3500799ec98128bcb9d8d14ac22f298769c14590a3e61828d3e5f8c',
    'deploy/mission-control/agent-runner/work_external_workflow.py': '3e0687fc5036c196ec10c1fda8a41a8fdc9c18604b5543c628540c08db471cb7',
    'deploy/mission-control/jenkins/verify-candidate.py': 'b9543660ae152263012f4e34420acc90429e407050f3a2fdafcde0725f397510',
    'deploy/mission-control/sql/apply.sql': 'bfdb898223d91ddc87584d12d6fe8e4cb7f74e92cee4c845a6975f69841010ee',
    'deploy/mission-control/sql/management/grant-external-workflows.sql': '2cd05f7532a7dde9153bdb2ae90f1a54e2f367676df977791536ec7d418ee62c',
    'deploy/mission-control/sql/management/verify-external-workflows.sql': '2bbfffb25b3a51e36ce01efc44afc9b5b26bfef26cfb2d650914be278dd0ab61',
    'deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql': '4e231e802726cde93a949d99b591aed3be3153abfa239bf7525959c24f49cf81',
    'docs/architecture/external-agent-workflow.md': 'c88bc59f6a4d40b462097f6c4fa0df1d1cdbbfe75c05b0a8382b85fb03e24faa',
    'docs/architecture/proposals/external-plan-approve-override.md': 'e92b81b24667616c915bdb18a7d1f2d4f3bc831bcadd0e43cdbe9d7192b6f31b',
}


# Audited task-recovery source. Prior maps remain historical provenance.
# Applied SQL and other legacy ownership guards are never qualified here.
TASK_RECOVERY_CONTEXT_SHA256 = {
    'AGENT_WORK.md': '829b24154ed8dcb5b868cb590c0c23c4dd5a7eefd6f2e93cd9848783340722b5',
    'deploy/mission-control/agent-runner/test_external_code_context_recovery.py': '3599f0dcd2b3e9c809cccc27e9de49b0b81529a6f59d751d625eba8f8cdb9825',
    'deploy/mission-control/agent-runner/test_external_diff_contract.py': 'b17eac7a806cec6abb408540bb9e29482fd895bcb6218ef6133dd5b1406f4383',
    'deploy/mission-control/agent-runner/test_native_deadline_events.py': '845725331ef2c9c298c069ba3d67b9aec1666bfb9d181589ecf47f50e81075bd',
    'deploy/mission-control/agent-runner/work_external_delivery.py': '90a5cf86892903c552500c49cebd8d6eac6e66b03142b03ad4efdcae67d32053',
    'deploy/mission-control/agent-runner/work_external_recovery.py': 'a51bce8d77e2ab0c8ca5680fe929ddac77237cbaf7377d9e5c0c5d1cfc3f4f4f',
    'deploy/mission-control/agent-runner/work_external_review_context.py': 'ef5000e360adf2ce223878e0c04556f6fd7318f46dd3af5660a910b2262ba6ee',
    'deploy/mission-control/agent-runner/work_external_workflow.py': '303f6a97a1fc94d3718a45f08aedab27dacdbcf38303220ff2f51c4562542213',
    'deploy/mission-control/agent-runner/work_workflow.py': '1af6d5e3ccdb0cc3c648065c28bffc0c00355424fcb5aee5350d88b3be131195',
    'src/rust/forge-ide-ui/src/model/agents/tests/scopes.rs': '0550f4b84191c4a214338f3b04fa7abeec7dbb2300c82f1b3dd64874a87bc4d7',
    'src/rust/forge-ide-ui/src/model/agents/workflow.rs': 'a38448c5ff0e2d7bce63d97dcbd0d234ef6b7f066511e32e0df4c9c1d646c8a3',
    'src/rust/forge-ide-web/tests/browser/agents-relay.mjs': '75eedd257d3429d30021775eb56ded22642900c0739348f0e9064dbf597a4986',
    'src/rust/forge-ide-web/tests/browser/journey.mjs': '8637cf0bde2a5adbbd2dfa37fe2e72797db823e2ad93e9e3b6bd58342662fc44',
}

TASK_RECOVERY_GUARD_AST_SHA256 = {
    'work_workflow.py::WorkflowStore.retry_native_timeout': '1dd378b0024d8ee49493672edb2f0a2d78d0e819033b62802efcdd2ddb7aab11',
}


# Already accepted on main57a0aca: exact M36 legacy-progress grandfathering
# verifier bytes. This qualifies no applied migration or other SQL authority.
M36_LEGACY_PROGRESS_VERIFIER_CONTEXT_SHA256 = {
    'deploy/mission-control/sql/management/verify.sql': '2cdd7ea50a8b646d2a603548a49bd8445524426ad5c12ebb5bef215a0a2c1dbf',
    'deploy/mission-control/sql/management/verify-task-events.sql': '942ab0db7b9f2e0cceaaf64d4d7c73fe16c9fa5cc306d4a7ba5b1f432b4510ed',
}


# Jenkins observation capability, separately selected from frozen mutations.
# Applied SQL, existing mutation configuration, and all prior literal maps stay
# unchanged. These are exact reviewed source bytes, never CI proof substitutes.
JENKINS_PAGED_OBSERVER_CONTEXT_SHA256 = {
    'AGENT_WORK.md': 'bffae2aaf43a78dac989f095e183b255662c92c155f115e0ba51e9a6846182e2',
    'deploy/mission-control/agent-runner/setup-runner.sh': '447042e4343851149c5f16a54a5f3ecb050205288fbb1f1105cd45f35b1f0631',
    'deploy/mission-control/agent-runner/test_jenkins_read_observer.py': '56e2699b08cf1d2a4b7bd9030d18eee6e161b288ad5acaf0cc5344e314840b42',
    'deploy/mission-control/agent-runner/work_external_delivery.py': '4f6d761911d3f0f3f3cac73b70cb69698017ac42b66757443aaba036a8706397',
    'deploy/mission-control/agent-runner/work_jenkins_observer.py': 'eab42d259be5934174208ba2b3842bc3198f86c34be061b532715991410a77de',
    'deploy/mission-control/agent-runner/work_publication.py': '7c553427acd4e1ca6802f6757148c5d8840a58e470291b443ce59be4e5e136a7',
    'src/rust/forge-jenkins/README.md': '2c8090f101956f122780fd39fef579daaa8b6dd068d192498d77794b0e24a0d2',
    'src/rust/forge-jenkins/src/client.rs': 'ada4f2d451a5a475d6996fc71298f0f86d5b5bdead5027b267c3a192201918b1',
    'src/rust/forge-jenkins/src/lib.rs': 'fb4c27de231b669da4cff69419387c932a83a6a96a735f76a5c50d92a4a7c46e',
    'src/rust/forge-jenkins/src/observation.rs': '4935d989cc43a89e1f871348fe080b75f8b331e3f47dfb78744de89ba5de30ce',
    'src/rust/forge-jenkins/tests/client.rs': '181344bfe0d8df65c53e03d7aba0e2f8f71953a3c5463c966e686464a72c14c8',
    'src/rust/forge-local/README.md': '2d65b2fba45d6497ceded1007201488abfaa1f4727bd215803572a45f38458da',
    'src/rust/forge-local/src/application.rs': '51cce8e196fe08714542ef73f9450e79e588f6846587ddfbe1b1022c0b792aa8',
    'src/rust/forge-local/src/jenkins.rs': '6411f099986858442339fff1406d899a51a3a2ae3d3f6c9e8c794a60fd1cfdc1',
    'src/rust/forge-local/tests/application.rs': '849d9793493fa8ac4501c4567ef67ab2c836d5cd1ada76385017dd1ea921f144',
    'src/rust/forge-local/tests/jenkins_application.rs': '34ab4f8c38133deef22780a3c00637d90f31db4ff277237b97a7a76943dcfd01',
    'src/rust/forge-terminal/README.md': '249c47a6d0bdc3dd7c551ca5f26296260504905e55cd8d0e5d06d96ce1b074ae',
    'src/rust/forge-terminal/src/jenkins.rs': '85536151556880d687d1df0c25b4cf27cffd87e3f7ff3be1d2d2cb023df3f3c2',
    'src/rust/forge-terminal/src/parser.rs': 'beb20d19ba5f566a6f75f333c1ca38bb01b6e618c744838f89e9722affeb5aaf',
    'src/rust/forge-terminal/tests/jenkins.rs': '7585207b7f1eef28fa9bb2118f9a40a432ba9f0703c8856d2058274fbb8fd3ed',
}


# Exact verification-only context registration; no SQL, gate contract,
# existing mutation transport, or historical source pin is changed.
NATIVE_CI_CONTEXT_RECOVERY_SHA256 = {
    'AGENT_WORK.md': 'c826ecfe8352eb1143993002c9acb74db3ff73a29ee8134646908e4659f4753c',
    'deploy/mission-control/agent-runner/setup-runner.sh': 'bc71e92930bc08907ad5be6598c17a312409232a653a0febcc1b0b267df076e9',
    'deploy/mission-control/agent-runner/test_native_ci_reprepare.py': 'ad1e395a60ccb09a495a15e48573b58df99d1eedc81eb47b458a2ae41e7674f0',
    'deploy/mission-control/agent-runner/work_native_ci_reprepare.py': '7a506685c0974e9cc29fe93416416538b79e71dfffe553bb2e8058cd08ca5db4',
}


# Audited opt-in content identity source; no applied SQL or legacy authority change.
CONTENT_IDENTITY_CONTEXT_SHA256 = {
    'AGENT_WORK.md': '020493751a8712e1bfff33e2578075cd312ff4e4f53dd01a3c69b0fb56759023',
    'deploy/mission-control/agent-runner/setup-runner.sh': '4a05e10774f0ced43ae482fff08c2f33a79666fa3e7c1cfc304f5cbacffc14b8',
    'deploy/mission-control/agent-runner/test_external_content_identity.py': 'da4e8a5f9562201271b15eccfa1fd6159ef69d32c91797013ec0093dd0f284e0',
    'deploy/mission-control/agent-runner/test_native_content_identity.py': '63d133ae4d6c3bed213ce00c48edd511c0a2a258f95a9b4b4c75457cff3d9d95',
    'deploy/mission-control/agent-runner/test_work_artifact_identity.py': 'ba2bfa1a6189f38c6e21295e1c08eba8c0c565e43651b038179f7d49b0dce64a',
    'deploy/mission-control/agent-runner/test_work_identity_store.py': '381b18cd7c9f519c0b16bb23a4848f0725f1b95515974871e6fe043ab58684e7',
    'deploy/mission-control/agent-runner/work_artifact_identity.py': 'dd10cf60a3116a49aacbbba28475a8141beffde5322f94d622c9e3ca0ad8a920',
    'deploy/mission-control/agent-runner/work_artifact_source.py': '3f900127a41b0c05c5054f78ba613e6f3b2b4de01b016811e935c4ba07c59dbc',
    'deploy/mission-control/agent-runner/work_config.py': '3206e73d5879a52feceadf1214ebd4ad42b288869c643b4a423a85bc7e0ba4df',
    'deploy/mission-control/agent-runner/work_external_delivery.py': '5f66919d4883791a57e8899ec349ca345ddb26fd3c4475dcf2a9c76b2318fcb4',
    'deploy/mission-control/agent-runner/work_external_identity.py': '951fe253d8b696a50602bfade28c735fe87d9846f6e011be610daa0855587504',
    'deploy/mission-control/agent-runner/work_external_review_context.py': 'e011a312a2efc6817815240089164cbc96910a1182730fe477d7c6a5dd284681',
    'deploy/mission-control/agent-runner/work_external_workflow.py': 'ab7c70ae0c996bd88a351f0dc2f47c43f5f56ff533e6a6ec2cc09efd7f273f97',
    'deploy/mission-control/agent-runner/work_identity_contract.py': '8173ce291194e4e43d7536636a1468ce43f20a2e67c631d26489bd37f9477735',
    'deploy/mission-control/agent-runner/work_identity_store.py': '2c7b18856d4e402477aa625ba04484053214eeda910ad82f099a0063fbc225b6',
    'deploy/mission-control/agent-runner/work_native_identity.py': '225fe528dceccf4fb72cf71568c4a10aeb602533c28918eed4e9aff5023f20e2',
    'deploy/mission-control/agent-runner/work_native_jenkins.py': '6cfd6527f60ee11b2adb9b32bf3a3343e49343a79b41aeffe9ffc80a4fdc1809',
    'deploy/mission-control/agent-runner/work_publication.py': '3802f8287ba078438a92869a51dd86a3990ceffbe97e0ba52d6ef6409328da5f',
    'deploy/mission-control/agent-runner/work_publication_config.py': '688af9e3c85fcb96e39c3e01d3767412ee0a44ce43c44b0668b5b2fa9b28d654',
    'deploy/mission-control/agent-runner/work_review.py': '1092d3017205e395be0c5bcb7f8a8af2cb2b7d6c87bf0c61578a9d128fd7d9f1',
    'deploy/mission-control/agent-runner/work_worker.py': '20eecb6e84cdffd34f5088ba864a91df23fd42bf30e1a7526a7912c22c9148da',
    'docs/README.md': 'eb87245ac52fcfcc5a029a116ee4a853f6de1dbbcf8e2e9a6c16be2230f91033',
    'docs/architecture/README.md': '1e44e052542a4ec8c8defbf779aaf9ac3ee501e29bb12c2c7bf509b2c7a1a622',
    'docs/architecture/content-identity.md': '433979bcf86e54d9bdf6c32566168cf0c233257c53f9dd2a0cef8e32abe50419',
    'docs/architecture/diagrams/mermaid/README.md': '422e1cffb92c7732ed87570884bdc0f693127a657b066ec4af2fe8775815277b',
    'docs/architecture/diagrams/mermaid/content-identity-components.mmd': 'c7cadb78d6a61b9a01c76f6cfbfae4a8d38a3823d1deea655f7340698e56a9d2',
    'docs/architecture/diagrams/mermaid/content-identity-data.mmd': '5cb75bd499e3f3e7ec0d6d1cad90becd53f6ab7e74c9e5001937bae924cfb1d2',
    'docs/architecture/diagrams/mermaid/content-identity-external.mmd': '55d499bc85a15f2dd12ce2664c7578c17e22f2aa9297a8c03fcb1ba82cb3a4ed',
    'docs/architecture/diagrams/mermaid/content-identity-native.mmd': 'ed112a4723e52f52981a5a36e4acf5ffc1430ce25b2abb376a2623f4c45c05be',
    'docs/architecture/diagrams/mermaid/images/content-identity-components.svg': '8c511c92eba56bed33856256ee9b7226d4b4c5367838f682760cc81c032818b0',
    'docs/architecture/diagrams/mermaid/images/content-identity-data.svg': '952c7f7353f4eb252baa419e8f1e4b02eb0b28d728538a5d4807057279707db8',
    'docs/architecture/diagrams/mermaid/images/content-identity-external.svg': '60f45da2224c301246ee0f7ab8fca53ae52bb26e38a3f612b6d995094e2575dd',
    'docs/architecture/diagrams/mermaid/images/content-identity-native.svg': '2f8d240fd63c7e47b79f45fdc01251ae358a6b5ced7bb41ff74aa0a3f6d95ca2',
    'docs/architecture/diagrams/mermaid/render-manifest.json': 'fcb4bbd8ab3ad4cbdf2599b6d841bf8afd45a18661dc590e7e73b1ebd9476527',
    'docs/architecture/diagrams/mermaid/render.py': '03b8519ab8d6349b22b1b151f6c6141c345b86a98fc55ea8498106865e8d6c05',
    'docs/architecture/diagrams/mermaid/validate.mjs': 'f131ed6cf60a8a9a53e065ed10f860b816560d4b673ea0c158ef346acc0f3a52',
    'docs/design/implementation-status.md': '2ce052d2ea0b72dc7848197902fcce89ee64fdde2f8e27832063a306d4d914c8',
    'src/rust/forge-ide-web-host/build.rs': '70cb9106edb7fa7ff95558df93e2b1d96a725bc530df6fe07a1efbb255939d59',
    'src/rust/forge-ide-web-host/src/assets.rs': '76e58af8b6879f8d42d98abace1aa839871e6773d7fcec7cd9d8dc9b713adbff',
    'src/rust/forge-ide-web-host/src/design/content_identity.rs': '4420c267a346145dfbc673464bb7c61999a475faeee201973a0e9a544fd6b674',
    'src/rust/forge-ide-web-host/src/design/landing.rs': 'a648264f98cab28ddac9a8e6c6885b62bb8f4145fd9088587fc7c56f2d70b0a5',
    'src/rust/forge-ide-web-host/src/design/mod.rs': '675f2d6a69507e739cb1246efc221a68e14a2b3f61ba1f28de048b987f514ac8',
    'src/rust/forge-ide-web-host/tests/consumer.rs': 'd27d35ae9bf1e8a5a4d8d89ed2addb5d844b99d56dc7d023e848a367ae9ce813',
}


# Scoped runner/verifier policy-parity and gate-manifest mismatch diagnostics;
# preserves all applied SQL, protected gate paths, and legacy authority AST pins.
VERIFIER_POLICY_PARITY_CONTEXT_SHA256 = {
    'deploy/mission-control/agent-runner/setup-runner.sh': 'a6cea972d0db842e32a38878887abb93140c6d87b48e7ae8660c5b54e51549cc',
    'deploy/mission-control/agent-runner/test_verifier_policy_parity.py': '1ca1c28b6b1ccd10b1c4edb99d875014e79d250c0a91dd4245d3046734af9ec0',
    'deploy/mission-control/agent-runner/test_work_artifact_identity.py': 'fe2066e23a7eb505c38de3d68da8846036ba55ada85710ab3477315784f77a99',
    'deploy/mission-control/agent-runner/work_ci_contract.py': '9746eb0484cc42677c05d8999c661d707ce62ee1d6c8c2e9a93bdfb1ea82a3c9',
    'deploy/mission-control/agent-runner/work_ci_phases.py': '5b1704aaf4580183db26a4d6dabfb3b0517529325c150eda2e04094bf62add68',
    'deploy/mission-control/agent-runner/work_delivery_repair.py': '193859469ddda803936086966aa0ede657ecae06636dabfb7dc5ae4516496160',
    'deploy/mission-control/agent-runner/work_external_delivery.py': '886615442806819fd9e4bba7b72bd0ffbd074e1f4137e8317e659e32866d40ce',
    'deploy/mission-control/agent-runner/work_native_jenkins.py': '9e2c73c569d21f28b506d0a63a633794f4e9bc4071232ed9cf174e4f1eae9ff3',
    'deploy/mission-control/jenkins/verify-candidate.py': 'ec9ddf29f8e6a6abe6bc7abf275abd6024218d1d2a2ab5fe318cfe7ea4d4e7f8',
}


def qualified_source(relative, expected):
    if relative in VERIFIER_POLICY_PARITY_CONTEXT_SHA256:
        return VERIFIER_POLICY_PARITY_CONTEXT_SHA256[relative]
    if relative in CONTENT_IDENTITY_CONTEXT_SHA256:
        return CONTENT_IDENTITY_CONTEXT_SHA256[relative]
    if relative in NATIVE_CI_CONTEXT_RECOVERY_SHA256:
        return NATIVE_CI_CONTEXT_RECOVERY_SHA256[relative]
    if relative in JENKINS_PAGED_OBSERVER_CONTEXT_SHA256:
        return JENKINS_PAGED_OBSERVER_CONTEXT_SHA256[relative]
    if relative in M36_LEGACY_PROGRESS_VERIFIER_CONTEXT_SHA256:
        return M36_LEGACY_PROGRESS_VERIFIER_CONTEXT_SHA256[relative]
    if relative in TASK_RECOVERY_CONTEXT_SHA256:
        return TASK_RECOVERY_CONTEXT_SHA256[relative]
    if relative in PLAN_APPROVE_OVERRIDE_CONTEXT_SHA256:
        return PLAN_APPROVE_OVERRIDE_CONTEXT_SHA256[relative]
    if relative in MUSE_MERGE_COMPATIBILITY_CONTEXT_SHA256:
        return MUSE_MERGE_COMPATIBILITY_CONTEXT_SHA256[relative]
    return PIPELINE_DIAGRAM_CONTEXT_SHA256.get(relative, UI_STAGE_DIAGRAM_CONTEXT_SHA256.get(relative, REVIEW_PROTOCOL_CONTEXT_SHA256.get(relative, PEER_FEEDBACK_CONTEXT_SHA256.get(relative, SUPERSEDING_REVIEW_CONTEXT_SHA256.get(relative, LINEAGE_RETRY_CONTEXT_SHA256.get(relative, EVENT_RETRY_CONTEXT_SHA256.get(relative, AGENT_WIDE_RETRY_CONTEXT_SHA256.get(relative, DELIVERY_AUTOMATION_CONTEXT_SHA256.get(relative, UI_DELIVERY_CONTEXT_SHA256.get(relative, POST_MERGE_CONTEXT_SHA256.get(relative, expected)))))))))))

STRICT_VERIFIER_FILES = {
    "verify-superseding-reviews.sql",
    "verify-delivery-lineage.sql",
    "verify-retry-events.sql",
    "verify-agent-retries.sql",
    "verify-delivery.sql",
    "verify-post-merge.sql",
    "verify.sql",
    "verify-owner-continuation.sql",
    "verify-agy-unattended.sql",
    "verify-native-jenkins.sql",
    "verify-native-delivery.sql",
    "verify-jenkins-events.sql",
    "verify-external-agent.sql",
    "verify-muse-read-access.sql",
    "verify-task-events.sql",
    "verify-external-workflows.sql",
    "verify-feedback.sql",
    "verify-autonomy.sql",
}


class JenkinsM27SourceBoundaryTest(unittest.TestCase):
    def test_applied_migrations_one_through_twenty_six_are_byte_identical(self):
        historical_files = {
            path.name for path in MIGRATIONS.glob("[0-9][0-9][0-9][0-9]_*.sql")
            if 1 <= int(path.name[:4]) <= 26
        }
        self.assertEqual(historical_files, set(MIGRATION_SHA256))
        for name, expected in MIGRATION_SHA256.items():
            with self.subTest(migration=name):
                self.assertEqual(hashlib.sha256((MIGRATIONS / name).read_bytes()).hexdigest(), expected)

    def test_unchanged_baseline_boundaries_remain_byte_identical(self):
        # An explicitly reviewed context may qualify current source bytes while
        # the historical baseline remains available for exact provenance.
        self.assertTrue(set(REVIEWED_CI_CONTEXT_SHA256) <= set(UNCHANGED_BASELINE_SHA256))
        current = {**UNCHANGED_BASELINE_SHA256, **REVIEWED_CI_CONTEXT_SHA256, **POST_MERGE_CONTEXT_SHA256, **UI_DELIVERY_CONTEXT_SHA256}
        for relative, expected in current.items():
            with self.subTest(source=relative):
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(),
                    qualified_source(relative, expected))

    def test_explicit_reviewer_context_matches_included_unapproved_source(self):
        for name, expected in INCLUDED_REVIEWER_CONTEXT_SHA256.items():
            with self.subTest(context=REVIEWER_CONTEXT_REVISION, source=name):
                self.assertEqual(hashlib.sha256((RUNNER / name).read_bytes()).hexdigest(),
                    qualified_source("deploy/mission-control/agent-runner/" + name, expected))

    def test_task_organization_changes_match_explicit_unapproved_context(self):
        for relative, expected in TASK_ORGANIZATION_CONTEXT_SHA256.items():
            with self.subTest(context='unapproved task organization', source=relative):
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(),
                    qualified_source(relative, expected))

    def test_retirement_and_ui_composition_match_their_explicit_source_contexts(self):
        for context, pins in ((RETIREMENT_CONTEXT_REVISION, INCLUDED_RETIREMENT_CONTEXT_SHA256),
                              ('consolidated task organization and retirement', CONSOLIDATED_UI_CONTEXT_SHA256)):
            for relative, expected in pins.items():
                with self.subTest(context=context, source=relative):
                    self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(),
                    qualified_source(relative, expected))

    def test_delivery_ui_overlay_is_scoped_to_reviewed_abi14_source(self):
        self.assertEqual(UI_DELIVERY_CONTEXT_REVISION, "f246b1494f29cf86af8a32a24da80d56c17427d1")
        self.assertEqual(len(UI_DELIVERY_CONTEXT_SHA256), 33)
        for relative, expected in UI_DELIVERY_CONTEXT_SHA256.items():
            with self.subTest(delivery_ui=relative):
                self.assertTrue(relative.startswith(("src/rust/forge-ide-ui/", "src/rust/forge-ide-web/", "src/rust/forge-ide-web-host/"))
                    or relative in {".github/workflows/ide-ui-browser.yml", "docs/architecture/forge-post-merge-verification.md", "docs/design/implementation-status.md"})
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))

    def test_delivery_automation_overlay_is_scoped_and_exact(self):
        # The pipeline diagram overlay supersedes this map for the files it
        # changed; the delivery hashes remain literal provenance, resolved
        # through qualified_source like the older overlay tests.
        for relative, expected in DELIVERY_AUTOMATION_CONTEXT_SHA256.items():
            with self.subTest(delivery_source=relative):
                self.assertTrue(relative.startswith(('deploy/mission-control/', 'src/rust/forge-ide-ui/', 'src/rust/forge-ide-web/', 'src/rust/forge-ide-web-host/')) or relative in {
                    'docs/architecture/forge-post-merge-verification.md', 'docs/design/implementation-status.md'})
                if '/migrations/' in relative:
                    self.assertTrue(relative.endswith('0029_delivery_automation.sql'))
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))

    def test_shared_retry_overlay_preserves_historical_authority_boundaries(self):
        self.assertEqual(set(AGENT_WIDE_RETRY_GUARD_AST_SHA256), {
            'work_owner_control.py::NativeOwnerControl.review_decide',
            'work_workflow.py::WorkflowStore.retry_peer_verdict',
            'work_workflow.py::WorkflowStore.retry_preparation',
        })
        for relative, expected in AGENT_WIDE_RETRY_CONTEXT_SHA256.items():
            with self.subTest(shared_retry_source=relative):
                self.assertTrue(relative.startswith(('deploy/mission-control/', 'src/rust/forge-ide-ui/', 'src/rust/forge-ide-web/'))
                    or relative in {'docs/architecture/forge-post-merge-verification.md', 'docs/design/implementation-status.md'})
                if '/migrations/' in relative:
                    self.assertTrue(relative.endswith('0030_agent_wide_retry_policy.sql'))
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))

    def test_event_retry_overlay_is_explicit_and_preserves_applied_migrations(self):
        self.assertEqual(set(EVENT_RETRY_GUARD_AST_SHA256), {
            'work_workflow.py::WorkflowStore.retry_peer_verdict'})
        for relative, expected in EVENT_RETRY_CONTEXT_SHA256.items():
            with self.subTest(event_retry_source=relative):
                self.assertTrue(relative.startswith(('deploy/mission-control/', 'src/rust/'))
                    or relative in {'docs/architecture/forge-post-merge-verification.md',
                        'docs/design/implementation-status.md'})
                if '/migrations/' in relative:
                    self.assertTrue(relative.endswith('0031_retry_events.sql'))
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))

    def test_lineage_overlay_preserves_applied_migrations_and_exact_source(self):
        for relative, expected in LINEAGE_RETRY_CONTEXT_SHA256.items():
            with self.subTest(lineage_source=relative):
                self.assertTrue(relative.startswith('deploy/mission-control/') or relative in {
                    'AGENT_WORK.md', 'docs/architecture/ci-stage-evidence.md'})
                if '/migrations/' in relative:
                    self.assertTrue(relative.endswith('0032_observer_retry_lineage.sql'))
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))
        for relative, expected in EVENT_RETRY_CONTEXT_SHA256.items():
            if '/migrations/0031_' in relative or '/fragments/0031_' in relative:
                with self.subTest(applied_m31=relative):
                    self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), expected)

    def test_superseding_review_overlay_preserves_applied_schema_and_exact_source(self):
        self.assertIn('deploy/mission-control/agent-runner/work_superseding_review.py', SUPERSEDING_REVIEW_CONTEXT_SHA256)
        for relative, expected in SUPERSEDING_REVIEW_CONTEXT_SHA256.items():
            with self.subTest(superseding_review_source=relative):
                self.assertTrue(relative.startswith('deploy/mission-control/') or relative in {
                    'AGENT_WORK.md', 'docs/architecture/proposals/forge-autonomous-agent-workflow.md'})
                if '/migrations/' in relative or '/fragments/' in relative:
                    self.assertTrue(relative.endswith('0033_superseding_reviews.sql'))
                actual = hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest()
                self.assertEqual(actual, expected if '/migrations/' in relative or '/fragments/' in relative else qualified_source(relative, expected))
        # The new context cannot qualify edits to any previously applied SQL.
        for pins in (EVENT_RETRY_CONTEXT_SHA256, LINEAGE_RETRY_CONTEXT_SHA256):
            for relative, expected in pins.items():
                if '/migrations/' in relative or '/fragments/' in relative:
                    with self.subTest(immutable_pre_m33=relative):
                        self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), expected)

    def test_peer_feedback_overlay_is_exact_and_cannot_qualify_applied_sql(self):
        self.assertEqual(set(PEER_FEEDBACK_CONTEXT_SHA256), {'docs/architecture/proposals/forge-autonomous-agent-workflow.md', 'deploy/mission-control/jenkins/verify-candidate.py', 'deploy/mission-control/agent-runner/work_peer_feedback_recovery.py', 'AGENT_WORK.md', 'deploy/mission-control/agent-runner/work_workflow.py', 'deploy/mission-control/agent-runner/test_work_peer_context.py', 'deploy/mission-control/agent-runner/test_work_peer_feedback_recovery.py', 'deploy/mission-control/agent-runner/work_peer_context.py', 'deploy/mission-control/agent-runner/test_ci_event_phases.py', 'deploy/mission-control/agent-runner/work_ci_contract.py', 'deploy/mission-control/agent-runner/work_context_recovery.py', 'deploy/mission-control/agent-runner/setup-runner.sh', 'deploy/mission-control/agent-runner/test_work_linked_child_feedback.py'})
        for relative, expected in PEER_FEEDBACK_CONTEXT_SHA256.items():
            with self.subTest(peer_feedback_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))
        for pins in (EVENT_RETRY_CONTEXT_SHA256, LINEAGE_RETRY_CONTEXT_SHA256, SUPERSEDING_REVIEW_CONTEXT_SHA256):
            for relative, expected in pins.items():
                if '/sql/' in relative:
                    with self.subTest(immutable_applied_sql=relative):
                        current = expected if '/migrations/' in relative or '/fragments/' in relative else qualified_source(relative, expected)
                        self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), current)

    def test_review_protocol_overlay_is_exact_and_preserves_applied_authority(self):
        self.assertEqual(set(REVIEW_PROTOCOL_CONTEXT_SHA256), {'deploy/mission-control/agent-runner/work_external_recovery.py', 'deploy/mission-control/agent-runner/work_executor.py', 'deploy/mission-control/agent-runner/work_superseding_review.py', 'deploy/mission-control/agent-runner/test_external_transport_recovery.py', 'deploy/mission-control/agent-runner/work_external_workflow.py', 'deploy/mission-control/agent-runner/test_ci_event_phases.py', 'docs/architecture/external-agent-workflow.md', 'AGENT_WORK.md', 'deploy/mission-control/jenkins/verify-candidate.py', 'deploy/mission-control/agent-runner/test_work_cli_transport.py', 'deploy/mission-control/agent-runner/test_work_review_revision_attestations.py', 'deploy/mission-control/agent-runner/setup-runner.sh', 'deploy/mission-control/agent-runner/work_worker.py', 'deploy/mission-control/agent-runner/README.md', 'deploy/mission-control/agent-runner/work_ci_contract.py', 'deploy/mission-control/agent-runner/work_cli_transport.py'})
        for relative, expected in REVIEW_PROTOCOL_CONTEXT_SHA256.items():
            with self.subTest(review_protocol_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))
        for pins in (EVENT_RETRY_CONTEXT_SHA256, LINEAGE_RETRY_CONTEXT_SHA256, SUPERSEDING_REVIEW_CONTEXT_SHA256):
            for relative, expected in pins.items():
                if '/migrations/' in relative or '/fragments/' in relative:
                    with self.subTest(applied_schema=relative):
                        self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), expected)

    def test_stage_diagram_ui_overlay_is_scoped_to_abi15_ui_source(self):
        self.assertEqual(len(UI_STAGE_DIAGRAM_CONTEXT_SHA256), 23)
        for relative, expected in UI_STAGE_DIAGRAM_CONTEXT_SHA256.items():
            with self.subTest(stage_diagram_ui=relative):
                self.assertTrue(
                    relative.startswith(('src/rust/forge-ide-ui/', 'src/rust/forge-ide-web/', 'src/rust/forge-ide-web-host/'))
                    or relative == 'docs/design/implementation-status.md'
                )
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))

    def test_pipeline_diagram_overlay_is_scoped_and_exact(self):
        self.assertEqual(len(PIPELINE_DIAGRAM_CONTEXT_SHA256), 52)
        for relative, expected in PIPELINE_DIAGRAM_CONTEXT_SHA256.items():
            with self.subTest(pipeline_source=relative):
                self.assertTrue(relative.startswith(('deploy/mission-control/', 'src/rust/forge-ide-ui/', 'src/rust/forge-ide-web/', 'src/rust/forge-ide-web-host/')) or relative in {
                    'docs/architecture/forge-post-merge-verification.md', 'docs/design/implementation-status.md'})
                if '/migrations/' in relative:
                    self.assertTrue(relative.endswith('0034_jenkins_stage_started.sql'))
                actual = hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest()
                self.assertEqual(actual, expected if '/migrations/' in relative else qualified_source(relative, expected))

    def test_muse_merge_compatibility_overlay_is_scoped_and_exact(self):
        self.assertEqual(set(MUSE_MERGE_COMPATIBILITY_CONTEXT_SHA256), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/test_superseding_review_schema.py',
            'deploy/mission-control/agent-runner/test_work_peer_diagnostics_schema.py',
            'deploy/mission-control/agent-runner/test_work_peer_replay.py',
            'deploy/mission-control/agent-runner/test_work_review_supersession_independent.py',
            'deploy/mission-control/agent-runner/work_workflow.py',
            'deploy/mission-control/sql/management/verify-autonomy.sql',
            'deploy/mission-control/sql/management/verify-superseding-reviews.sql',
            'deploy/mission-control/sql/migrations/0035_event_stream_peer_review_diagnostics.sql',
        })
        for relative, expected in MUSE_MERGE_COMPATIBILITY_CONTEXT_SHA256.items():
            with self.subTest(muse_compatibility_source=relative):
                if '/migrations/' in relative or '/fragments/' in relative:
                    self.assertEqual(relative, 'deploy/mission-control/sql/migrations/0035_event_stream_peer_review_diagnostics.sql')
                actual = hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest()
                self.assertEqual(actual, expected if '/migrations/' in relative or '/fragments/' in relative else qualified_source(relative, expected))

    def test_plan_approve_override_overlay_is_scoped_and_preserves_applied_migrations(self):
        self.assertIn('deploy/mission-control/agent-runner/work_external_plan_override.py', PLAN_APPROVE_OVERRIDE_CONTEXT_SHA256)
        self.assertIn('deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql', PLAN_APPROVE_OVERRIDE_CONTEXT_SHA256)
        for relative, expected in PLAN_APPROVE_OVERRIDE_CONTEXT_SHA256.items():
            with self.subTest(plan_override_source=relative):
                self.assertTrue(
                    relative.startswith('deploy/mission-control/')
                    or relative in {
                        'docs/architecture/external-agent-workflow.md',
                        'docs/architecture/proposals/external-plan-approve-override.md',
                    }
                )
                if '/migrations/' in relative or '/fragments/' in relative:
                    self.assertEqual(relative, 'deploy/mission-control/sql/migrations/0038_external_plan_approve_override.sql')
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), expected if '/migrations/' in relative or '/fragments/' in relative else qualified_source(relative, expected))
        published = {
            tuple(shlex.split(line, comments=True))
            for line in (RUNNER / 'setup-runner.sh').read_text().splitlines()
            if line.lstrip().startswith('publish ')
        }
        self.assertIn(('publish', '$directory/work_external_plan_override.py', '$target/work_external_plan_override.py'), published)

    def test_task_recovery_overlay_is_exact_and_keeps_other_guards(self):
        self.assertEqual(set(TASK_RECOVERY_CONTEXT_SHA256), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/test_external_code_context_recovery.py',
            'deploy/mission-control/agent-runner/test_external_diff_contract.py',
            'deploy/mission-control/agent-runner/test_native_deadline_events.py',
            'deploy/mission-control/agent-runner/work_external_delivery.py',
            'deploy/mission-control/agent-runner/work_external_recovery.py',
            'deploy/mission-control/agent-runner/work_external_review_context.py',
            'deploy/mission-control/agent-runner/work_external_workflow.py',
            'deploy/mission-control/agent-runner/work_workflow.py',
            'src/rust/forge-ide-ui/src/model/agents/tests/scopes.rs',
            'src/rust/forge-ide-ui/src/model/agents/workflow.rs',
            'src/rust/forge-ide-web/tests/browser/agents-relay.mjs',
            'src/rust/forge-ide-web/tests/browser/journey.mjs',
        })
        self.assertEqual(set(TASK_RECOVERY_GUARD_AST_SHA256), {
            'work_workflow.py::WorkflowStore.retry_native_timeout'})
        for relative, expected in TASK_RECOVERY_CONTEXT_SHA256.items():
            with self.subTest(task_recovery_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))

    def test_m36_legacy_progress_verifier_context_is_exact(self):
        self.assertEqual(set(M36_LEGACY_PROGRESS_VERIFIER_CONTEXT_SHA256), {
            'deploy/mission-control/sql/management/verify.sql',
            'deploy/mission-control/sql/management/verify-task-events.sql',
        })
        for relative, expected in M36_LEGACY_PROGRESS_VERIFIER_CONTEXT_SHA256.items():
            with self.subTest(accepted_m36_verifier=relative):
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), expected)

    def test_jenkins_paged_observer_context_is_scoped_and_exact(self):
        self.assertEqual(set(JENKINS_PAGED_OBSERVER_CONTEXT_SHA256), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/setup-runner.sh',
            'deploy/mission-control/agent-runner/test_jenkins_read_observer.py',
            'deploy/mission-control/agent-runner/work_external_delivery.py',
            'deploy/mission-control/agent-runner/work_jenkins_observer.py',
            'deploy/mission-control/agent-runner/work_publication.py',
            'src/rust/forge-jenkins/README.md',
            'src/rust/forge-jenkins/src/client.rs',
            'src/rust/forge-jenkins/src/lib.rs',
            'src/rust/forge-jenkins/src/observation.rs',
            'src/rust/forge-jenkins/tests/client.rs',
            'src/rust/forge-local/README.md',
            'src/rust/forge-local/src/application.rs',
            'src/rust/forge-local/src/jenkins.rs',
            'src/rust/forge-local/tests/application.rs',
            'src/rust/forge-local/tests/jenkins_application.rs',
            'src/rust/forge-terminal/README.md',
            'src/rust/forge-terminal/src/jenkins.rs',
            'src/rust/forge-terminal/src/parser.rs',
            'src/rust/forge-terminal/tests/jenkins.rs',
        })
        for relative, expected in JENKINS_PAGED_OBSERVER_CONTEXT_SHA256.items():
            with self.subTest(jenkins_paged_observer_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))

    def test_native_ci_context_recovery_is_scoped_and_exact(self):
        self.assertEqual(set(NATIVE_CI_CONTEXT_RECOVERY_SHA256), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/setup-runner.sh',
            'deploy/mission-control/agent-runner/test_native_ci_reprepare.py',
            'deploy/mission-control/agent-runner/work_native_ci_reprepare.py',
        })
        for relative, expected in NATIVE_CI_CONTEXT_RECOVERY_SHA256.items():
            with self.subTest(native_ci_context_source=relative):
                self.assertNotIn("/sql/", relative)
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))

    def test_content_identity_context_is_scoped_and_exact(self):
        self.assertEqual(set(CONTENT_IDENTITY_CONTEXT_SHA256), {
            'AGENT_WORK.md',
            'deploy/mission-control/agent-runner/setup-runner.sh',
            'deploy/mission-control/agent-runner/test_external_content_identity.py',
            'deploy/mission-control/agent-runner/test_native_content_identity.py',
            'deploy/mission-control/agent-runner/test_work_artifact_identity.py',
            'deploy/mission-control/agent-runner/test_work_identity_store.py',
            'deploy/mission-control/agent-runner/work_artifact_identity.py',
            'deploy/mission-control/agent-runner/work_artifact_source.py',
            'deploy/mission-control/agent-runner/work_config.py',
            'deploy/mission-control/agent-runner/work_external_delivery.py',
            'deploy/mission-control/agent-runner/work_external_identity.py',
            'deploy/mission-control/agent-runner/work_external_review_context.py',
            'deploy/mission-control/agent-runner/work_external_workflow.py',
            'deploy/mission-control/agent-runner/work_identity_contract.py',
            'deploy/mission-control/agent-runner/work_identity_store.py',
            'deploy/mission-control/agent-runner/work_native_identity.py',
            'deploy/mission-control/agent-runner/work_native_jenkins.py',
            'deploy/mission-control/agent-runner/work_publication.py',
            'deploy/mission-control/agent-runner/work_publication_config.py',
            'deploy/mission-control/agent-runner/work_review.py',
            'deploy/mission-control/agent-runner/work_worker.py',
            'docs/README.md',
            'docs/architecture/README.md',
            'docs/architecture/content-identity.md',
            'docs/architecture/diagrams/mermaid/README.md',
            'docs/architecture/diagrams/mermaid/content-identity-components.mmd',
            'docs/architecture/diagrams/mermaid/content-identity-data.mmd',
            'docs/architecture/diagrams/mermaid/content-identity-external.mmd',
            'docs/architecture/diagrams/mermaid/content-identity-native.mmd',
            'docs/architecture/diagrams/mermaid/images/content-identity-components.svg',
            'docs/architecture/diagrams/mermaid/images/content-identity-data.svg',
            'docs/architecture/diagrams/mermaid/images/content-identity-external.svg',
            'docs/architecture/diagrams/mermaid/images/content-identity-native.svg',
            'docs/architecture/diagrams/mermaid/render-manifest.json',
            'docs/architecture/diagrams/mermaid/render.py',
            'docs/architecture/diagrams/mermaid/validate.mjs',
            'docs/design/implementation-status.md',
            'src/rust/forge-ide-web-host/build.rs',
            'src/rust/forge-ide-web-host/src/assets.rs',
            'src/rust/forge-ide-web-host/src/design/content_identity.rs',
            'src/rust/forge-ide-web-host/src/design/landing.rs',
            'src/rust/forge-ide-web-host/src/design/mod.rs',
            'src/rust/forge-ide-web-host/tests/consumer.rs',
        })
        for relative, expected in CONTENT_IDENTITY_CONTEXT_SHA256.items():
            with self.subTest(content_identity_source=relative):
                self.assertNotIn("/sql/", relative)
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), qualified_source(relative, expected))
        published={tuple(shlex.split(line,comments=True)) for line in (RUNNER / "setup-runner.sh").read_text().splitlines() if line.lstrip().startswith("publish ")}
        for name in ("work_artifact_identity.py","work_artifact_source.py","work_identity_contract.py","work_identity_store.py","work_native_identity.py","work_external_identity.py"):
            self.assertIn(("publish","$directory/"+name,"$target/"+name),published)

    def test_verifier_policy_parity_overlay_is_scoped_and_preserves_applied_sql(self):
        self.assertEqual(set(VERIFIER_POLICY_PARITY_CONTEXT_SHA256), {
            'deploy/mission-control/agent-runner/setup-runner.sh',
            'deploy/mission-control/agent-runner/test_verifier_policy_parity.py',
            'deploy/mission-control/agent-runner/test_work_artifact_identity.py',
            'deploy/mission-control/agent-runner/work_ci_contract.py',
            'deploy/mission-control/agent-runner/work_ci_phases.py',
            'deploy/mission-control/agent-runner/work_delivery_repair.py',
            'deploy/mission-control/agent-runner/work_external_delivery.py',
            'deploy/mission-control/agent-runner/work_native_jenkins.py',
            'deploy/mission-control/jenkins/verify-candidate.py',
        })
        for relative, expected in VERIFIER_POLICY_PARITY_CONTEXT_SHA256.items():
            with self.subTest(verifier_policy_parity_source=relative):
                self.assertNotIn('/sql/', relative)
                self.assertEqual(hashlib.sha256((REPO_ROOT / relative).read_bytes()).hexdigest(), expected)

    def test_retired_browser_reducers_and_dispatchers_are_absent(self):
        for relative in RETIRED_BROWSER_SOURCE_FILES:
            with self.subTest(retired=relative):
                self.assertFalse((REPO_ROOT / relative).exists())

    def test_legacy_owner_and_publication_guards_remain_ast_identical(self):
        for identity, expected in UNCHANGED_LEGACY_GUARD_AST_SHA256.items():
            expected = AGENT_WIDE_RETRY_GUARD_AST_SHA256.get(identity, expected)
            expected = EVENT_RETRY_GUARD_AST_SHA256.get(identity, expected)
            expected = TASK_RECOVERY_GUARD_AST_SHA256.get(identity, expected)
            filename, qualified = identity.split('::')
            node = ast.parse((RUNNER / filename).read_text())
            for name in qualified.split('.'):
                node = next(child for child in node.body if getattr(child, 'name', None) == name)
            with self.subTest(guard=identity):
                self.assertEqual(hashlib.sha256(ast.dump(node, include_attributes=False).encode()).hexdigest(), expected)

    def test_strict_verifier_closure_retains_owner_and_adds_jenkins(self):
        pending = [MANAGEMENT / "verify.sql"]
        visited = set()
        while pending:
            source = pending.pop().resolve()
            if source in visited:
                continue
            self.assertTrue(source.is_relative_to(MANAGEMENT.resolve()), source)
            self.assertTrue(source.is_file(), source)
            visited.add(source)
            for name in re.findall(r"^\\ir\s+(\S+)\s*$", source.read_text(), re.MULTILINE):
                pending.append(source.parent / name)
        self.assertEqual({source.name for source in visited}, STRICT_VERIFIER_FILES)

    def test_normal_installer_preserves_owner_control_and_installs_event_service(self):
        published = {
            tuple(shlex.split(line, comments=True))
            for line in (RUNNER / "setup-runner.sh").read_text().splitlines()
            if line.lstrip().startswith("publish ")
        }
        required = (
            "work_owner_control.py", "work_owner_reverification.py", "work_publication_clients.py", "work_review.py",
            "work_store.py", "work_worker_store.py", "work_reviewer_correction.py", "work_superseding_review.py",
            "work_peer_feedback_recovery.py", "work_cli_transport.py",
            "work_post_merge.py", "work_post_merge_provider.py", "jenkins_events.py", "jenkins_config.py",
            "jenkins_codec.py", "jenkins_store.py", "jenkins_consumer.py",
            "jenkins_api.py", "jenkins_emitter.py", "jenkins_emit_tool.py",
            "work_jenkins_lifecycle.py", "work_pipeline_projection.py",
        )
        for name in required:
            with self.subTest(module=name):
                self.assertTrue((RUNNER / name).is_file())
                self.assertIn(("publish", "$directory/" + name, "$target/" + name), published)


if __name__ == "__main__":
    unittest.main()
