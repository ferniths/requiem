path = r'C:\Users\Fern\Desktop\superproductiveharnessthing-cli\crates\orchestration\src\agent_runtime.rs'
with open(path, 'r', encoding='utf-8') as f:
    src = f.read()
src = src.replace(
    'use requiem_core::message::AgentMessage;',
    '',
    1
)
with open(path, 'w', encoding='utf-8') as f:
    f.write(src)
print('done')
