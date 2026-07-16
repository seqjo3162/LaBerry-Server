// Тарифы подписок
const SUBSCRIPTION_PLANS = [
    {
        id: 'basic',
        name: 'Berry Plus',
        price: 249,
        features: ['Расширенные реакции', 'Акцент профиля', 'Бейдж подписчика'],
        berryBonusPercent: 0
    },
    {
        id: 'premium',
        name: 'Berry Ultra',
        price: 349,
        features: ['GIF-избранное без лимита', 'Подарочные месяцы', 'Приоритетные функции', '+15% Berry к каждому пополнению баланса'],
        berryBonusPercent: 15
    }
];

// Текущий режим и выбранный план
let currentSubscriptionMode = 'personal';
let selectedPlanId = SUBSCRIPTION_PLANS[0].id;

// HTML-карточка одного плана
function planCardHtml(plan) {
    const isSelected = plan.id === selectedPlanId;
    return `
    <div class="subscription-plan ${isSelected ? 'selected' : ''}" data-plan-id="${plan.id}">
      <h3>${plan.name}</h3>
      <div class="plan-price">${plan.price} ₽ / мес</div>
      <ul>${plan.features.map(f => `<li>${f}</li>`).join('')}</ul>
      ${plan.berryBonusPercent > 0 ? `<div class="badge bonus-badge">+${plan.berryBonusPercent}% к пополнению Berry</div>` : ''}
    </div>
  `;
}

// Выпадающий список серверов для режима поддержки
function serverOptionsHtml() {
    const servers = (Array.isArray(lastServersSnapshot) ? lastServersSnapshot : [])
        .filter(s => Number(s?.id) > 0);

    if (!servers.length) return '<option value="">Нет доступных серверов</option>';

    return servers
        .map(s => `<option value="${s.id}">${escapeHtml(s.name || `Сервер ${s.id}`)}</option>`)
        .join('');
}

// Отрисовка всей секции подписок
export function renderSubscriptionPanel(mode = 'personal') {
    currentSubscriptionMode = mode;
    const isServer = mode === 'server';

    const plansHtml = SUBSCRIPTION_PLANS.map(planCardHtml).join('');

    const checkoutBody = isServer
        ? `
        <label class="subscription-field">
          <span>Сервер для поддержки</span>
          <select class="inp" id="subscriptionServerSelect">
            ${serverOptionsHtml()}
          </select>
        </label>
        <button class="btn btn-primary" type="button" id="subscriptionPayBtn">Поддержать</button>
      `
        : `
        <label class="subscription-option"><input type="radio" name="subTarget" value="self" checked /> <span>Купить себе</span></label>
        <label class="subscription-option"><input type="radio" name="subTarget" value="gift" /> <span>Подарить подписку</span></label>
        <input class="inp" id="subscriptionGiftInput" placeholder="Ник получателя подарка" autocomplete="off" hidden />
        <button class="btn btn-primary" type="button" id="subscriptionPayBtn">Перейти к оплате</button>
      `;

    return `
    <section class="utility-shell subscription-shell">
      <div class="utility-hero">
        <div>
          <div class="utility-kicker">Подписка</div>
          <h2>${isServer ? 'Поддержать сервер' : 'Личная подписка'}</h2>
          <p>${isServer
            ? 'Помощь серверу, бусты и видимые бонусы для сообщества.'
            : 'Выберите план и получите преимущества.'}</p>
        </div>
        <div class="subscription-switch">
          <button class="${isServer ? 'active' : ''}" type="button" data-sub-mode="server">Поддержать сервер</button>
          <button class="${!isServer ? 'active' : ''}" type="button" data-sub-mode="personal">Личная подписка</button>
        </div>
      </div>

      <div class="subscription-layout">
        <div class="subscription-plans">
          ${plansHtml}
        </div>

        <div class="subscription-checkout-card">
          <div class="subscription-checkout-title">${isServer ? 'Поддержка сервера' : 'Оформление'}</div>
          ${checkoutBody}
          <div class="subscription-payment-methods">
            <label class="subscription-option"><input type="radio" name="paymentMethod" value="qr" checked /> <span>Оплата по QR-Code</span></label>
            <label class="subscription-option"><input type="radio" name="paymentMethod" value="card" /> <span>Карта через провайдера</span></label>
          </div>
          <div class="subscription-qr-box" id="subscriptionQrBox">
            <div class="subscription-qr-mark">QR</div>
            <span>Безопасный сценарий: код открывает платёжную страницу провайдера.</span>
          </div>
          <div class="subscription-note">Платёж будет обработан через ЮMoney.</div>
        </div>
      </div>
    </section>
  `;
}

// Привязка обработчиков событий после вставки HTML
export function initSubscriptionEvents(container) {
    // Переключение режима (личная / сервер)
    container.querySelectorAll('[data-sub-mode]').forEach(btn => {
        btn.addEventListener('click', () => {
            const mode = btn.dataset.subMode;
            if (typeof openUtilityPanel === 'function') {
                openUtilityPanel('subscription', { mode });
            } else {
                console.error('openUtilityPanel не найдена');
            }
        });
    });

    // Выбор плана
    container.querySelectorAll('.subscription-plan').forEach(card => {
        card.addEventListener('click', () => {
            container.querySelectorAll('.subscription-plan').forEach(c => c.classList.remove('selected'));
            card.classList.add('selected');
            selectedPlanId = card.dataset.planId;
        });
    });

    // Показать/скрыть поле ввода ника для подарка
    const giftInput = container.querySelector('#subscriptionGiftInput');
    container.querySelectorAll('input[name="subTarget"]').forEach(radio => {
        radio.addEventListener('change', () => {
            const isGift = radio.value === 'gift';
            if (giftInput) giftInput.hidden = !isGift;
            if (!isGift && giftInput) giftInput.value = '';
        });
    });

    // Кнопка оплаты
    container.querySelector('#subscriptionPayBtn')?.addEventListener('click', () => {
        handleSubscriptionPayment(container);
    });
}

// Отправка запроса на создание платежа
async function handleSubscriptionPayment(container) {
    const planId = selectedPlanId;
    const mode = currentSubscriptionMode;
    const paymentMethod = container.querySelector('input[name="paymentMethod"]:checked')?.value || 'qr';

    let giftTo = null;
    if (mode === 'personal') {
        const target = container.querySelector('input[name="subTarget"]:checked')?.value;
        if (target === 'gift') {
            giftTo = container.querySelector('#subscriptionGiftInput')?.value?.trim();
            if (!giftTo) {
                showToast('Введите ник получателя');
                return;
            }
        }
    }

    let serverId = null;
    if (mode === 'server') {
        serverId = container.querySelector('#subscriptionServerSelect')?.value;
        if (!serverId) {
            showToast('Выберите сервер');
            return;
        }
    }

    try {
        const response = await fetch('/api/payments/create', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({
                plan_id: planId,
                mode,
                payment_method: paymentMethod,
                gift_to: giftTo,
                server_id: serverId
            })
        });

        if (!response.ok) throw new Error('Ошибка создания платежа');

        const data = await response.json();
        if (paymentMethod === 'qr' && data.qr_url) {
            showQrCode(data.qr_url);
        } else if (data.payment_url) {
            window.open(data.payment_url, '_blank');
        } else {
            showToast('Неизвестный ответ от сервера');
        }
    } catch (err) {
        console.error(err);
        showToast('Не удалось создать платёж');
    }
}