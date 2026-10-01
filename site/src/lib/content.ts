export const repository = 'https://github.com/yaojie-shen/Skills-Manager-TUI';
export const installCommand = 'curl -fsSL https://github.com/yaojie-shen/Skills-Manager-TUI/raw/main/install.sh | sh';
export const captureTabs = ['library', 'tags', 'presets', 'agents', 'repos', 'health'] as const;

const captureNames = { library: 'Library', tags: 'Tags', presets: 'Presets', agents: 'Agents', repos: 'Repos', health: 'Health' };

export const copy = {
  en: {
    lang: 'en', title: 'Skills Manager: Manage and deploy Agent Skills',
    description: 'Keep Agent Skills in one local Library and link them to configured agent directories from the terminal.',
    skip: 'Skip to content', guide: 'Documentation', source: 'Source', menu: 'Menu', navigation: 'Main navigation',
    theme: 'Change color theme', light: 'Light', dark: 'Dark', system: 'System',
    eyebrow: 'Skills Manager', heroTitle: 'Manage and deploy Agent Skills from the terminal',
    intro: 'Keep skills in one local Library and link the ones you need to Claude Code, Codex, or another configured agent directory.',
    start: 'Install Skills Manager', quickstartLink: 'Quick start',
    capture: {
      label: 'TUI',
      title: 'TUI screens',
      intro: 'Library, tags, presets, Agents, repositories, and health checks share one terminal interface.',
      names: captureNames,
      screens: {
        library: { title: 'Library and skill details', text: 'SKILL.md, source, presets, and deployment status.' },
        tags: { title: 'Skills by tag', text: 'Current Library members in each tag.' },
        presets: { title: 'Preset membership', text: 'Fixed members and tag coverage.' },
        agents: { title: 'Agent directories', text: 'Global and project directories by Agent.' },
        repos: { title: 'Skill sources', text: 'Local, Git, and URL repositories.' },
        health: { title: 'Library health', text: 'Invalid, modified, missing, and broken links.' },
      },
    },
    workflowCode: 'CLI', workflowLabel: 'Agent interface', workflowTitle: 'CLI interface',
    workflowIntro: 'Agents use the CLI to inspect the Library, organize skills, and deploy them.',
    steps: [
      { id: '01', phase: 'SEARCH', title: 'Search the Library', text: 'Free text and filters narrow the Library to matching skills.', command: 'skills list "incident response" tag:operations', output: 'incident-triage  local                   [operations]  Triage production incidents with safe checklists\n                 ↳ [name,description,tag] incident response runbook and safe checklists', slug: 'library', link: 'Library search' },
      { id: '02', phase: 'ORGANIZE', title: 'Tags and presets', text: 'Tags support retrieval; presets define a fixed deployment set.', command: 'skills tag add incident-triage operations', output: 'incident-triage: operations', slug: 'tags-presets', link: 'Tag and preset commands' },
      { id: '03', phase: 'DEPLOY', title: 'Deploy a skill', text: 'Link a Library skill into an Agent directory.', command: 'skills deploy incident-triage --agent claude', output: 'link   claude/incident-triage -> ~/.skills/local/incident-triage\napplied 1 change(s)', slug: 'deployment', link: 'Deployment commands' },
    ],
    runbookCode: 'INSTALL', installLabel: 'Installation', installTitle: 'Install and initialize the Library',
    installIntro: '',
    installHeading: 'Install', copy: 'Copy', copied: 'Copied', copyFailed: 'Select and copy the command manually.',
    initialize: 'Create the Library', initEnv: '$SKILLS_HOME', initNote: 'is required here to set the Library path.',
    installationLink: 'All installation options', footerLine: 'Skills Manager', colophonType: 'DOCUMENTATION',
    footerGuide: 'Documentation', license: 'MIT',
  },
  'zh-cn': {
    lang: 'zh-CN', title: 'Skills Manager：管理和部署 Agent Skills',
    description: '将 Agent Skills 保存在一个本地 Library 中，并通过终端链接到已配置的 Agent 目录。',
    skip: '跳转到正文', guide: '使用文档', source: '源代码', menu: '菜单', navigation: '主导航',
    theme: '切换颜色主题', light: '浅色', dark: '深色', system: '跟随系统',
    eyebrow: 'Skills Manager', heroTitle: '在终端中管理和部署 Agent Skills',
    intro: '将技能保存在一个本地 Library 中，并把所需技能链接到 Claude Code、Codex 或其他已配置的 Agent 目录。',
    start: '安装 Skills Manager', quickstartLink: '快速开始',
    capture: {
      label: 'TUI',
      title: 'TUI 页面', intro: 'Library、标签、预设、Agent、仓库和健康检查集中在同一个终端界面中。',
      names: captureNames,
      screens: {
        library: { title: 'Library 与技能详情', text: 'SKILL.md、来源、预设和部署状态。' },
        tags: { title: '按标签归类', text: '各标签中的 Library 成员。' },
        presets: { title: '预设成员', text: '固定成员和标签覆盖范围。' },
        agents: { title: 'Agent 目录', text: '各 Agent 的全局和项目目录。' },
        repos: { title: '技能来源', text: '本地、Git 和 URL 仓库。' },
        health: { title: 'Library 健康状态', text: '无效、已修改、缺失和断链。' },
      },
    },
    workflowCode: 'CLI', workflowLabel: 'Agent 接口', workflowTitle: 'CLI 接口',
    workflowIntro: 'Agent 使用 CLI 检查 Library、整理技能并部署所需内容。',
    steps: [
      { id: '01', phase: '搜索', title: '搜索 Library', text: '自由文本和筛选条件可以缩小技能范围。', command: 'skills list "incident response" tag:operations', output: 'incident-triage  local                   [operations]  Triage production incidents with safe checklists\n                 ↳ [name,description,tag] incident response runbook and safe checklists', slug: 'library', link: 'Library 搜索' },
      { id: '02', phase: '整理', title: '标签与预设', text: '标签用于检索，预设用于定义固定的部署集合。', command: 'skills tag add incident-triage operations', output: 'incident-triage: operations', slug: 'tags-presets', link: '标签与预设命令' },
      { id: '03', phase: '部署', title: '部署技能', text: '将 Library 技能链接到 Agent 目录。', command: 'skills deploy incident-triage --agent claude', output: 'link   claude/incident-triage -> ~/.skills/local/incident-triage\napplied 1 change(s)', slug: 'deployment', link: '部署命令' },
    ],
    runbookCode: '安装', installLabel: '安装', installTitle: '安装并初始化 Library',
    installIntro: '',
    installHeading: '安装', copy: '复制', copied: '已复制', copyFailed: '请手动选择并复制命令。',
    initialize: '创建 Library', initEnv: '$SKILLS_HOME', initNote: '用于在此处设置 Library 路径。',
    installationLink: '全部安装选项', footerLine: 'Skills Manager', colophonType: '使用文档',
    footerGuide: '使用文档', license: 'MIT',
  },
};
